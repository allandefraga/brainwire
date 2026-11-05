"""Pure diagnostics tests; no Docker, graph, credentials or network."""
import json
import os
import shlex
import argparse
import contextlib
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import types
import unittest
from unittest.mock import Mock, patch

import cgroup_pressure_smoke as smoke


def raw_snapshot():
    return {'uptime': '12.34', 'memory.current': '100', 'memory.max': '512',
            'memory.peak': '101', 'memory.swap.max': '0', 'memory.high': 'max',
            'inactive_file_values': ['10'],
            'events': [['low', '0'], ['high', '0'], ['max', '0'], ['oom', '0'],
                       ['oom_kill', '0'], ['oom_group_kill', '0']]}


class ObserverParserTests(unittest.TestCase):
    def test_valid_snapshot_preserves_raw_and_effective_readings(self):
        result = smoke.parse_observer_snapshot(json.dumps(raw_snapshot()))
        self.assertEqual(result['memory.current'], 100)
        self.assertEqual(result['memory.max'], 512)
        self.assertEqual(result['diagnostic_working_set']['bytes'], 90)
        self.assertEqual(result['observer_uptime_seconds'], '12.34')
        self.assertEqual(result['memory.events']['oom_kill'], 0)

    def test_invalid_missing_and_duplicate_readings_are_inconclusive(self):
        cases = []
        for bad in ([], ['10', '20'], ['-1'], ['1KiB'], ['18446744073709551616']):
            value = raw_snapshot(); value['inactive_file_values'] = bad; cases.append(value)
        for bad in ('-1', 'true', '', '1 extra', '18446744073709551616', True):
            value = raw_snapshot(); value['memory.current'] = bad; cases.append(value)
        for key in raw_snapshot():
            value = raw_snapshot(); del value[key]; cases.append(value)
        for bad in ([['oom', '0'], ['oom', '1']], [['oom', '-1']], [['bad name', '0']], []):
            value = raw_snapshot(); value['events'] = bad; cases.append(value)
        for value in cases:
            with self.subTest(value=value), self.assertRaises(smoke.SmokeInconclusive):
                smoke.parse_observer_snapshot(json.dumps(value))
        for text in ('not json', '{}', '{"uptime":"1","uptime":"2"}', '[]'):
            with self.subTest(text=text), self.assertRaises(smoke.SmokeInconclusive):
                smoke.parse_observer_snapshot(text)

    def test_bounded_reader_rejects_partial_timeout_eof_and_oversize(self):
        for data, close, message in ((b'partial', False, 'timed out'),
                                      (b'partial', True, 'closed'),
                                      (b'x' * (smoke.OBSERVER_MAX_BYTES + 1), True, 'limit')):
            read_fd, write_fd = os.pipe()
            try:
                os.set_blocking(read_fd, False)
                if len(data) > 4096:
                    thread = threading.Thread(target=lambda: (os.write(write_fd, data), os.close(write_fd)))
                    thread.start(); writer = None
                else:
                    os.write(write_fd, data)
                    writer = write_fd
                    if close: os.close(write_fd); writer = None
                with self.assertRaisesRegex(smoke.SmokeInconclusive, message):
                    smoke.observer_readline(read_fd, .05)
                if len(data) > 4096: thread.join(timeout=1)
            finally:
                os.close(read_fd)
                if writer is not None: os.close(writer)

    def test_complete_line_and_cancellation_are_bounded(self):
        read_fd, write_fd = os.pipe()
        try:
            os.set_blocking(read_fd, False); os.write(write_fd, b'{"ok":true}\n')
            self.assertEqual(smoke.observer_readline(read_fd, .1), b'{"ok":true}')
            stopped = threading.Event(); stopped.set()
            with self.assertRaises(smoke.SmokeInconclusive):
                smoke.observer_readline(read_fd, .1, stopped)
        finally:
            os.close(read_fd); os.close(write_fd)

    def test_second_unavailable_read_cannot_reuse_the_previous_snapshot(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            files = {'uptime': '12.34 10.00\n', 'memory.current': '100\n',
                     'memory.max': '512\n', 'memory.peak': '101\n', 'memory.swap.max': '0\n',
                     'memory.high': 'max\n', 'memory.stat': 'inactive_file 10\n',
                     'memory.events': 'oom 0\noom_kill 0\noom_group_kill 0\n'}
            script = smoke.OBSERVER_SCRIPT
            for name, value in files.items():
                path = root / name; path.write_text(value)
                original = '/proc/uptime' if name == 'uptime' else '/sys/fs/cgroup/' + name
                script = script.replace(original, shlex.quote(str(path)))
            process = subprocess.Popen(['/bin/sh', '-c', script], stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            try:
                os.set_blocking(process.stdout.fileno(), False)
                process.stdin.write(b'sample\n'); process.stdin.flush()
                self.assertEqual(smoke.parse_observer_snapshot(smoke.observer_readline(process.stdout.fileno(), 1))['memory.current'], 100)
                (root / 'memory.current').unlink()
                process.stdin.write(b'sample\n'); process.stdin.flush()
                with self.assertRaises(smoke.SmokeInconclusive):
                    smoke.observer_readline(process.stdout.fileno(), 1)
                self.assertNotEqual(process.wait(timeout=1), 0)
            finally:
                if process.poll() is None:
                    process.terminate(); process.wait(timeout=1)
                process.stdin.close(); process.stdout.close()


class ObserverOwnershipTests(unittest.TestCase):
    def test_wrong_ownership_never_starts_observer(self):
        with patch.object(smoke, 'inspect_owned', side_effect=RuntimeError('unowned')), \
             patch.object(smoke.subprocess, 'Popen') as popen:
            with self.assertRaises(RuntimeError):
                smoke.PersistentCgroupObserver('server', 'mine')
        popen.assert_not_called()


class PipeProcess:
    """In-process protocol peer for lifecycle tests, without Docker."""
    def __init__(self):
        child_read, parent_write = os.pipe()
        parent_read, child_write = os.pipe()
        self.stdin, self.stdout = os.fdopen(parent_write, 'wb', 0), os.fdopen(parent_read, 'rb', 0)
        self.requests, self.stopped = 0, False
        def reply():
            try:
                with os.fdopen(child_read, 'rb') as stream:
                    for line in stream:
                        if line != b'sample\n':
                            break
                        self.requests += 1
                        os.write(child_write, json.dumps(raw_snapshot()).encode() + b'\n')
            finally:
                os.close(child_write)
                self.stopped = True
        self.thread = threading.Thread(target=reply, daemon=True)
        self.thread.start()

    def poll(self):
        return 0 if self.stopped else None

    def terminate(self):
        self.stdin.close()

    def kill(self):
        self.stdin.close()

    def wait(self, timeout):
        self.thread.join(timeout)
        if self.thread.is_alive():
            raise subprocess.TimeoutExpired('fake observer', timeout)
        return 0


class ObserverLifecycleTests(unittest.TestCase):
    @contextlib.contextmanager
    def observer(self):
        process = PipeProcess()
        with patch.object(smoke, 'inspect_owned', return_value={'Id': 'immutable'}), \
             patch.object(smoke.subprocess, 'Popen', return_value=process) as popen:
            observer = smoke.PersistentCgroupObserver('server', 'mine')
            try:
                yield observer, process, popen
            finally:
                observer.close()
                self.assertFalse(process.thread.is_alive())

    def test_many_samples_use_one_owned_process_and_reject_a_changed_target(self):
        with self.observer() as (observer, process, popen):
            for _ in range(5):
                self.assertEqual(observer.sample('server')['memory.current'], 100)
            self.assertEqual(process.requests, 5)
            popen.assert_called_once()
            self.assertIn('immutable', popen.call_args.args[0])
            with self.assertRaises(smoke.SmokeInconclusive):
                observer.sample('another-server')
            self.assertEqual(process.requests, 5)

    def test_failed_nonblocking_configuration_stops_the_started_process(self):
        process = PipeProcess()
        with patch.object(smoke, 'inspect_owned', return_value={'Id': 'immutable'}), \
             patch.object(smoke.subprocess, 'Popen', return_value=process), \
             patch.object(smoke.os, 'set_blocking', side_effect=OSError('unsupported')):
            with self.assertRaises(smoke.SmokeInconclusive):
                smoke.PersistentCgroupObserver('server', 'mine')
        self.assertFalse(process.thread.is_alive())
        self.assertTrue(process.stdin.closed)
        self.assertTrue(process.stdout.closed)

    def test_window_stops_before_keyboard_interrupt_leaves_scope(self):
        with self.observer() as (observer, _, _):
            with self.assertRaises(KeyboardInterrupt):
                with observer.window('transaction_run') as window:
                    raise KeyboardInterrupt()
            self.assertEqual(window['phase'], 'transaction_run')
            self.assertLessEqual(window['operation_monotonic_start_ns'], window['operation_monotonic_end_ns'])
            self.assertFalse(any(t.name == 'brainwire-cgroup-observer' for t in threading.enumerate()))
            self.assertEqual(observer.sample('server')['memory.current'], 100)

    def test_window_caps_samples_and_retained_bytes(self):
        for field, limit, expected in (('OBSERVER_MAX_SAMPLES', 1, 'SampleLimitReached'),
                                        ('OBSERVER_MAX_WINDOW_BYTES', 1, 'SampleByteLimitReached')):
            with self.observer() as (observer, _, _), patch.object(smoke, field, limit):
                with observer.window() as window:
                    deadline = time.monotonic() + 1
                    while not window.get('observer_error') and time.monotonic() < deadline:
                        time.sleep(.002)
                self.assertEqual(window.get('observer_error'), expected)
                self.assertLessEqual(len(window['samples']), 1)
                self.assertFalse(any(t.name == 'brainwire-cgroup-observer' for t in threading.enumerate()))

    def test_sample_failure_after_operation_ends_remains_inconclusive(self):
        class Refused(Exception):
            code = smoke.PRESSURE_CODE
            message = ('pressure [brainwire.memory reason=cgroup requested_bytes=0 '
                       'available_bytes=0 rolled_back=true]')
        ready, stopped = threading.Event(), threading.Event()
        real_event = threading.Event
        first = [True]
        def events():
            if first:
                first.pop()
                return stopped
            return real_event()
        def bad_sample(*args, **kwargs):
            ready.set(); stopped.wait(1)
            raise smoke.SmokeInconclusive('invalid in-flight sample')
        def operation():
            self.assertTrue(ready.wait(1))
            raise Refused()
        with self.observer() as (observer, _, _), \
             patch.object(observer, 'sample', side_effect=bad_sample), \
             patch.object(smoke.threading, 'Event', side_effect=events):
            with self.assertRaises(smoke.SmokeInconclusive):
                smoke.require_pressure_error('server', operation, [], sample=lambda *a, **kw: {}, observer=observer)
            self.assertEqual(observer.windows[0].get('observer_error'), 'SmokeInconclusive')

    def test_failed_wait_still_closes_both_observer_pipes(self):
        observer = smoke.PersistentCgroupObserver.__new__(smoke.PersistentCgroupObserver)
        observer.closed = threading.Event()
        observer.process = Mock()
        observer.process.poll.return_value = None
        observer.process.wait.side_effect = subprocess.TimeoutExpired('observer', 3)
        with self.assertRaises(subprocess.TimeoutExpired):
            observer.close()
        observer.process.kill.assert_called_once()
        observer.process.stdin.close.assert_called_once()
        observer.process.stdout.close.assert_called_once()


class ObserverVerdictTests(unittest.TestCase):
    def test_trace_timestamps_do_not_prove_decision_time_pressure(self):
        snapshot = {'memory.max': smoke.LIMIT, 'diagnostic_working_set': {'bytes': smoke.LIMIT}}
        checks = []
        with self.assertRaises(smoke.SmokeInconclusive):
            smoke.require_pressure_error('server', lambda: None, checks,
                                         sample=lambda *a, **kw: snapshot, trace=True)
        self.assertLessEqual(checks[0]['operation_wall_start_ns'], checks[0]['operation_wall_end_ns'])

    def failing_window(self):
        @contextlib.contextmanager
        def window(phase=None):
            yield {'observer_error': 'SmokeInconclusive', 'samples': []}
        return types.SimpleNamespace(window=window)

    def test_observer_failure_never_turns_admission_or_refusal_into_pass(self):
        class Refused(Exception):
            code = smoke.PRESSURE_CODE
            message = ('pressure [brainwire.memory reason=cgroup requested_bytes=0 '
                       'available_bytes=0 rolled_back=true]')
        def refused():
            raise Refused()
        for operation in (lambda: None, refused):
            with self.subTest(operation=operation), self.assertRaises(smoke.SmokeInconclusive):
                smoke.require_pressure_error('server', operation, [], sample=lambda *a, **kw: {},
                                             observer=self.failing_window())
        def wrong_error():
            raise ValueError('functional failure')
        with self.assertRaises(AssertionError):
            smoke.require_pressure_error('server', wrong_error, [], sample=lambda *a, **kw: {},
                                         observer=self.failing_window())

    def test_after_snapshot_failure_does_not_hide_a_functional_error(self):
        @contextlib.contextmanager
        def window(phase=None):
            yield {'samples': []}
        observer = types.SimpleNamespace(window=window)
        def wrong_error():
            raise ValueError('functional failure')
        sample = Mock(side_effect=[{}, smoke.SmokeInconclusive('after snapshot timed out')])
        with self.assertRaises(AssertionError):
            smoke.require_pressure_error('server', wrong_error, [], sample=sample, observer=observer)

    def test_oom_and_cleanup_still_dominate_an_inconclusive_observer(self):
        for extra in ({'cleanup_cgroup': {'memory.events': {'oom': 1}}},
                      {'owned_resources_cleaned': False}):
            def run(args, report):
                self.assertTrue(args.persistent_observer)
                report.update(extra)
                raise smoke.SmokeInconclusive('observer timed out')
            with tempfile.TemporaryDirectory() as folder:
                output = Path(folder) / 'receipt.json'
                with patch.object(sys, 'argv', ['smoke', '--image', 'local', '--persistent-observer', '--output', str(output)]), \
                     patch.object(smoke, 'run', side_effect=run), contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(smoke.main(), 1)
                self.assertEqual(json.loads(output.read_text())['result'], 'FAIL')

    def test_default_is_unchanged_and_output_is_exclusive_before_any_resources(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / 'receipt.json'
            def run(args, report):
                self.assertFalse(args.persistent_observer)
                self.assertFalse(args.trace_admission)
                report['result'] = 'PASS'
            with patch.object(sys, 'argv', ['smoke', '--image', 'local', '--output', str(output)]), \
                 patch.object(smoke, 'run', side_effect=run), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(smoke.main(), 0)
            with patch.object(sys, 'argv', ['smoke', '--image', 'local', '--persistent-observer', '--output', str(output)]), \
                 patch.object(smoke, 'run') as mocked:
                with self.assertRaises(FileExistsError):
                    smoke.main()
                mocked.assert_not_called()

    def test_trace_option_is_explicit(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / 'receipt.json'
            def run(args, report):
                self.assertTrue(args.trace_admission)
                report['result'] = 'PASS'
            with patch.object(sys, 'argv', ['smoke', '--image', 'local', '--trace-admission', '--output', str(output)]), \
                 patch.object(smoke, 'run', side_effect=run), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(smoke.main(), 0)

    def test_run_tears_down_observer_before_resource_cleanup_on_errors_and_interrupts(self):
        for failure, close_failure in ((smoke.SmokeInconclusive('bad sample'), False),
                                       (KeyboardInterrupt(), False),
                                       (smoke.SmokeInconclusive('bad sample'), True)):
            order = []
            image = 'sha256:' + 'a' * 64
            state = {'Id': 'immutable', 'Image': image, 'State': {'Running': True},
                     'NetworkSettings': {'Ports': {'7687/tcp': [{'HostPort': '17687'}]}}}
            driver = Mock()
            driver.__enter__ = Mock(return_value=driver)
            driver.__exit__ = Mock(return_value=False)
            neo4j = types.SimpleNamespace(GraphDatabase=types.SimpleNamespace(driver=Mock(return_value=driver)), __version__='test')
            observer = Mock(windows=[])
            observer.sample.side_effect = failure
            def close():
                order.append('close')
                if close_failure:
                    raise RuntimeError('cannot stop observer')
            observer.close.side_effect = close
            def docker(*args, **kwargs):
                return json.dumps([{'Id': image}]) if args[:2] == ('image', 'inspect') else ''
            report = {'result': 'FAIL'}
            def finalize(*args):
                order.append('cleanup')
                report['owned_resources_cleaned'] = True
            with patch.dict(sys.modules, {'neo4j': neo4j}), patch.object(smoke, 'docker', side_effect=docker), \
                 patch.object(smoke, 'inspect_owned', return_value=state), patch.object(smoke.subprocess, 'Popen'), \
                 patch.object(smoke, 'PersistentCgroupObserver', return_value=observer), \
                 patch.object(smoke, 'finalize', side_effect=finalize):
                with self.assertRaises(type(failure)):
                    smoke.run(argparse.Namespace(image='local', persistent_observer=True), report)
            self.assertEqual(order, ['close', 'cleanup'])
            if close_failure:
                self.assertFalse(report['owned_resources_cleaned'])
                self.assertEqual(report['result'], 'FAIL')
                self.assertEqual(report['cleanup_errors'][0]['operation'], 'stop persistent observer')

    def test_missing_container_is_inconclusive_before_exec(self):
        with patch.object(smoke, 'inspect_owned', return_value=None), \
             patch.object(smoke.subprocess, 'Popen') as popen:
            with self.assertRaises(smoke.SmokeInconclusive):
                smoke.PersistentCgroupObserver('server', 'mine')
        popen.assert_not_called()


if __name__ == '__main__':
    unittest.main()
