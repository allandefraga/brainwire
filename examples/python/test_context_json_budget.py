import json
import tracemalloc
import unittest
from enum import IntEnum

from brainwire.context import ContextLimitError, _check_json_size


class ContextJsonBudgetTests(unittest.TestCase):
    def test_preflight_preserves_earlier_json_errors_and_counts_numeric_subclasses(self):
        with self.assertRaises(TypeError):
            _check_json_size([object()], 1, 'too large')
        with self.assertRaises(TypeError):
            _check_json_size([object(), 'x' * 4096], 128, 'too large')
        with self.assertRaises(ContextLimitError):
            _check_json_size(['🙂' * 40, object()], 128, 'too large')
        with self.assertRaises(TypeError):
            _check_json_size({('unsupported',): 'value'}, 128, 'too large')

        class Number(IntEnum):
            ZERO = 0

        with self.assertRaises(ContextLimitError):
            _check_json_size([Number.ZERO] * 20000, 128, 'too large')

    def test_utf8_exact_boundary_and_early_rejection(self):
        value = {'text': 'é🙂', 'control': '\x00\n"\\', 'nested': [1, None, True]}
        size = len(json.dumps(value, ensure_ascii=False, allow_nan=False).encode('utf-8'))
        self.assertEqual(_check_json_size(value, size, 'too large'), size)
        with self.assertRaisesRegex(ContextLimitError, 'too large'):
            _check_json_size(value, size - 1, 'too large')
        with self.assertRaisesRegex(ContextLimitError, 'too large'):
            _check_json_size({'large': 'x' * 4096, 'later': object()}, 512, 'too large')
        with self.assertRaises(ValueError):
            _check_json_size({'score': float('nan')}, 512, 'too large')
        with self.assertRaises(UnicodeEncodeError):
            _check_json_size({'text': '\x00' * 20 + '\ud800'}, 10, 'too large')
        with self.assertRaises(TypeError):
            _check_json_size({'unsupported': object()}, 512, 'too large')
        cycle = []
        cycle.append(cycle)
        with self.assertRaisesRegex(ValueError, 'Circular reference detected'):
            _check_json_size(cycle, 512, 'too large')

    def test_chunk_boundaries_preserve_exact_utf8_and_escape_sizes(self):
        for length in (4095, 4096, 4097, 8192, 8193):
            value = ('é\n"' * (length // 3 + 1))[:length]
            expected = len(json.dumps(value, ensure_ascii=False).encode('utf-8'))
            with self.subTest(length=length):
                self.assertEqual(_check_json_size(value, expected, 'too large'), expected)
                with self.assertRaises(ContextLimitError):
                    _check_json_size(value, expected - 1, 'too large')

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires independent allocation tracing')
    def test_many_chunks_do_not_materialize_the_entire_serialized_output(self):
        value = {'records': [{'text': 'é' * 4096} for _ in range(100)]}
        encoded_size = len(json.dumps(value, ensure_ascii=False).encode('utf-8'))
        tracemalloc.start()
        try:
            _check_json_size(value, encoded_size, 'too large')
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertLess(peak, encoded_size // 2)

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires independent allocation tracing')
    def test_lower_bound_stops_at_the_budget_for_deep_and_shared_values(self):
        deep = 'x'
        for _ in range(20000):
            deep = [deep]
        shared = []
        for _ in range(30):
            shared = [shared, shared]

        for value in (deep, shared):
            tracemalloc.start()
            try:
                with self.assertRaises(ContextLimitError):
                    _check_json_size(value, 128, 'too large')
                _, peak = tracemalloc.get_traced_memory()
            finally:
                tracemalloc.stop()
            self.assertLess(peak, 1 << 20)

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires independent allocation tracing')
    def test_deep_values_do_not_build_unbounded_preflight_stack(self):
        value = 'x'
        for _ in range(20000):
            value = [value]
        for budget in (1 << 16, 1 << 20):
            tracemalloc.start()
            try:
                with self.assertRaises(ContextLimitError):
                    _check_json_size(value, budget, 'too large')
                _, peak = tracemalloc.get_traced_memory()
            finally:
                tracemalloc.stop()
            self.assertLess(peak, 3 << 20)

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires independent allocation tracing')
    def test_deep_large_leaf_is_rejected_without_materialization(self):
        value = 'x' * (8 << 20)
        for _ in range(300):
            value = [value]
        tracemalloc.start()
        try:
            with self.assertRaises(ContextLimitError):
                _check_json_size(value, 1 << 16, 'too large')
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 3 << 20)

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires independent allocation tracing')
    def test_non_string_json_keys_do_not_bypass_nested_string_preflight(self):
        value = {0: 'x' * (8 << 20)}
        tracemalloc.start()
        try:
            with self.assertRaises(ContextLimitError):
                _check_json_size(value, 128, 'too large')
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 1 << 20)

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires independent allocation tracing')
    def test_escape_expansion_is_preflighted_without_an_encoded_string_copy(self):
        value = {'text': '\x00' * 220000}
        tracemalloc.start()
        try:
            with self.assertRaises(ContextLimitError):
                _check_json_size(value, 1 << 20, 'too large')
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 1 << 20)


if __name__ == '__main__':
    unittest.main()
