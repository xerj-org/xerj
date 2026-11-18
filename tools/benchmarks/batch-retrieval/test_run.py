import importlib.util
import unittest
import tempfile
from pathlib import Path

spec = importlib.util.spec_from_file_location('batch_eval', Path(__file__).with_name('run.py'))
eval = importlib.util.module_from_spec(spec)
spec.loader.exec_module(eval)


def hit(identifier='known', citation_id=None):
    return {'citation': {'_index': 'advisories', '_id': citation_id or identifier},
            'hit': {'_index': 'advisories', '_id': identifier, '_source': {'id': identifier}}}


class ScoringTests(unittest.TestCase):
    def test_executed_snapshot_does_not_reopen_mutable_binary(self):
        with tempfile.TemporaryDirectory() as temp:
            work = Path(temp)
            original = work / 'input'
            original.write_bytes(b'first binary')
            data = original.read_bytes()
            original.write_bytes(b'replacement binary')
            frozen = eval.freeze_binary(data, work)
            self.assertEqual(frozen.read_bytes(), data)
            self.assertEqual(eval.sha(frozen.read_bytes()), eval.sha(data))
            self.assertTrue(frozen.stat().st_mode & 0o100)

    def test_wrong_archive_is_refused_before_parsing(self):
        with tempfile.TemporaryDirectory() as temp:
            pack = Path(temp) / 'pack.zip'
            pack.write_bytes(b'untrusted archive')
            with self.assertRaisesRegex(ValueError, 'SHA256 mismatch'):
                eval.read_pack(pack)

    def test_grouping_counts_exact_wire_and_preserves_duplicates(self):
        cases = [{'kind': 'missing', 'input': {'key': 'same', 'identifier': 'no', 'field': 'id'},
                  'expected_ids': []}] * 2
        def call(args):
            self.assertEqual(args["max_tokens"], 1_000_000)
            response = {'results': [{'position': i, 'key': 'same', 'status': 'abstain',
                                     'abstain': True, 'reason': 'no_hits'}
                                    for i in range(len(args['queries']))]}
            return eval.wire(args), eval.wire(response), response, 1.0
        class Encoding:
            def encode(self, text, **kwargs):
                return list(text)
        batch, _ = eval.trial(call, cases, True, Encoding())
        single, _ = eval.trial(call, cases, False, Encoding())
        self.assertEqual((len(batch['calls']), len(single['calls'])), (1, 2))
        self.assertEqual(len(batch['items']), 2)
        for result in (batch, single):
            self.assertTrue(all(i['correct'] for i in result['items']))
            for receipt in result['calls']:
                self.assertEqual(receipt['input_bpe_tokens'], receipt['request_bytes'])
                self.assertEqual(receipt['output_bpe_tokens'], receipt['response_bytes'])

    def test_budgeted_evidence_is_refused(self):
        with self.assertRaisesRegex(ValueError, 'full evidence'):
            eval.ordered_rows({'_token_budget': {}, 'results': []}, [])

    def test_wrong_id_is_not_correct(self):
        case = {'input': {'key': 'q'}, 'kind': 'canonical', 'expected_ids': ['known']}
        row = {'status': 'hits', 'abstain': False, 'hits': [hit('wrong')]}
        self.assertFalse(eval.score(case, row)['correct'])

    def test_error_is_never_a_miss(self):
        case = {'input': {'key': 'q'}, 'kind': 'missing', 'expected_ids': []}
        self.assertFalse(eval.score(case, {'status': 'error', 'error': 'timeout'})['correct'])

    def test_citation_must_identify_actual_hit(self):
        with self.assertRaisesRegex(ValueError, 'citation'):
            eval.score({'input': {'key': 'q'}, 'kind': 'canonical', 'expected_ids': ['known']},
                       {'status': 'hits', 'abstain': False, 'hits': [hit(citation_id='wrong')]})

    def test_empty_miss_and_expected_item_error_are_separate(self):
        self.assertTrue(eval.score({'input': {'key': 'q'}, 'kind': 'missing', 'expected_ids': []},
                                  {'status': 'abstain', 'abstain': True, 'reason': 'no_hits'})['correct'])
        self.assertTrue(eval.score({'input': {'key': 'q'}, 'kind': 'invalid', 'expected_ids': []},
                                  {'status': 'error', 'error': 'bad query'})['correct'])

    def test_extra_wrong_hit_fails_exact_lookup(self):
        self.assertFalse(eval.score({'input': {'key': 'q'}, 'kind': 'alias', 'expected_ids': ['known']},
                                    {'status': 'hits', 'abstain': False,
                                     'hits': [hit(), hit('wrong')]})['correct'])

    def test_text_uses_labelled_hit_at_k(self):
        self.assertTrue(eval.score({'input': {'key': 'q'}, 'kind': 'text', 'expected_ids': ['known']},
                                  {'status': 'hits', 'abstain': False,
                                   'hits': [hit('other'), hit()]})['correct'])

    def test_reordered_or_missing_positions_refused(self):
        with self.assertRaisesRegex(ValueError, 'position'):
            eval.ordered_rows({'results': [{'position': 1}]}, [{'key': 'x'}])
        with self.assertRaisesRegex(ValueError, 'count'):
            eval.ordered_rows({'results': []}, [{'key': 'x'}])

    def test_different_ranked_evidence_fails_parity(self):
        a = [{'status': 'hits', 'hits': [hit()]}]
        b = [{'status': 'hits', 'hits': [hit('wrong')]}]
        self.assertNotEqual(eval.evidence_signature(a), eval.evidence_signature(b))


if __name__ == '__main__':
    unittest.main()
