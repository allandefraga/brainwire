import json
import tracemalloc
import unittest

from brainwire.context import ContextLimitError, _check_json_size


class ContextJsonBudgetTests(unittest.TestCase):
    def test_utf8_exact_boundary_and_early_rejection(self):
        value = {'text': 'é🙂', 'nested': [1, None, True]}
        size = len(json.dumps(value, ensure_ascii=False, allow_nan=False).encode('utf-8'))
        _check_json_size(value, size, 'too large')
        with self.assertRaisesRegex(ContextLimitError, 'too large'):
            _check_json_size(value, size - 1, 'too large')
        with self.assertRaisesRegex(ContextLimitError, 'too large'):
            _check_json_size({'large': 'x' * 4096, 'later': object()}, 512, 'too large')
        with self.assertRaises(ValueError):
            _check_json_size({'score': float('nan')}, 512, 'too large')
        with self.assertRaises(TypeError):
            _check_json_size({'unsupported': object()}, 512, 'too large')

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


if __name__ == '__main__':
    unittest.main()
