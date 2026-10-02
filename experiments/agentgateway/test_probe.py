"""Regression checks for false-positive fixture acceptance, without networking."""
import copy
from email.message import Message
import json
from pathlib import Path
import unittest
import probe


class AcceptanceTests(unittest.TestCase):
    def setUp(self):
        self.results = json.loads(Path(__file__).with_name('results.json').read_text())

    def test_known_observations_are_accepted(self):
        probe.validate_results(self.results)
        for measurement in self.results['paths'].values():
            measurement['terminal_id_count'] = tuple(measurement['terminal_id_count'])
        probe.validate_results(self.results)

    def test_functional_regressions_are_rejected(self):
        regressions = [
            ('chat_json', 'status', 503),
            ('responses_json', 'contains_expected_text', False),
            ('messages_sse', 'status', 502),
            ('chat_sse', 'total_ms', 1),
            ('bridge_json', 'actual_upstream_path', '/v1/chat/completions'),
            ('bridge_sse', 'contains_expected_text', False),
            ('responses_cancel', 'upstream_cancelled', False),
            ('chat_cancel', 'upstream_attempts', 2),
            ('messages_cancel', 'status', 502),
            ('chat_missing', 'wire_usage', {'prompt_tokens': 0}),
            ('responses_missing', 'status', 502),
            ('error', 'upstream_attempts', 1),
            ('malformed', 'status', 200),
            ('timeout', 'total_ms', 35000),
        ]
        for path in ('direct', 'gateway'):
            for case, field, value in regressions:
                with self.subTest(path=path, case=case, field=field):
                    changed = copy.deepcopy(self.results)
                    changed['paths'][path]['cases'][case][field] = value
                    with self.assertRaises(RuntimeError):
                        probe.validate_results(changed)

    def test_gateway_messages_missing_usage_limit_is_explicit(self):
        self.results['paths']['gateway']['cases']['messages_missing']['status'] = 200
        with self.assertRaises(RuntimeError):
            probe.validate_results(self.results)

    def test_duplicate_terminal_is_rejected(self):
        self.results['paths']['gateway']['terminal_id_count'][2] -= 1
        with self.assertRaises(RuntimeError):
            probe.validate_results(self.results)

    def test_one_leaking_fixture_request_is_rejected(self):
        self.results['fixture_records'][0]['auth_ok'] = False
        with self.assertRaises(RuntimeError):
            probe.validate_results(self.results)


class CredentialTests(unittest.TestCase):
    def headers(self, **values):
        headers = Message()
        for key, value in values.items():
            headers[key.replace('_', '-')] = value
        return headers

    def test_correct_protocol_headers_are_accepted(self):
        for path in ('/v1/chat/completions', '/v1/responses'):
            self.assertTrue(probe.fixture_auth_ok(path, self.headers(Authorization='Bearer '+probe.KEY)))
        self.assertTrue(probe.fixture_auth_ok('/v1/messages', self.headers(x_api_key=probe.KEY)))

    def test_wrong_protocol_header_is_rejected(self):
        self.assertFalse(probe.fixture_auth_ok('/v1/chat/completions', self.headers(x_api_key=probe.KEY)))
        self.assertFalse(probe.fixture_auth_ok('/v1/messages', self.headers(Authorization='Bearer '+probe.KEY)))

    def test_valid_provider_header_cannot_hide_client_key(self):
        for path, good, bad in [
            ('/v1/chat/completions', {'Authorization':'Bearer '+probe.KEY}, {'x_api_key':'fake-client-key'}),
            ('/v1/responses', {'Authorization':'Bearer '+probe.KEY}, {'x_api_key':'fake-client-key'}),
            ('/v1/messages', {'x_api_key':probe.KEY}, {'Authorization':'Bearer fake-client-key'}),
        ]:
            with self.subTest(path=path):
                self.assertFalse(probe.fixture_auth_ok(path, self.headers(**good, **bad)))
        headers = self.headers(Authorization='Bearer '+probe.KEY)
        headers['Authorization'] = 'Bearer fake-client-key'
        self.assertFalse(probe.fixture_auth_ok('/v1/chat/completions', headers))


if __name__ == '__main__':
    unittest.main()
