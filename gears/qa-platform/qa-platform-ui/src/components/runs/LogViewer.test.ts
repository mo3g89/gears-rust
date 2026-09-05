// Proves the log stream is split into one section per test.
//
// The regression this guards: this file was copied from the source system,
// whose runner brackets each test with `=== TEST_FILE: ===` /
// `=== TEST_RESULT: <title> PASSED ===`. Ours emits neither -- it runs pytest
// once and emits `=== TEST_CASE: <base64 json> ===` per test. With only the
// legacy patterns nothing ever closed a block, so all 19 tests of a real run
// collapsed into a single "Run setup" section.
//
// The payloads below are REAL, taken verbatim from the am-validation-smoke-1
// pod log on the live cluster, not hand-written to match the parser.
import { describe, expect, it } from 'vitest';
import { buildBlocks } from './LogViewer';

function marker(payload: Record<string, unknown>): string {
  return `=== TEST_CASE: ${btoa(JSON.stringify(payload))} ===`;
}

const PASSED = {
  nodeid: 'tests/account-management/test_am_smoke.py::test_bootstrap_creates_root_tenant',
  file: 'tests/account-management/test_am_smoke.py',
  name: 'test_bootstrap_creates_root_tenant',
  outcome: 'passed',
  duration: 0.152,
  reason: null,
};
const FAILED = { ...PASSED, name: 'test_create_tenant_rejects_unknown_field', outcome: 'failed' };
const SKIPPED = { ...PASSED, name: 'test_skipped_one', outcome: 'skipped' };

describe('buildBlocks', () => {
  it('gives each test its own section, titled and status-dotted', () => {
    const blocks = buildBlocks([
      'runner: image=qa-platform-pytest-runner',
      'collecting ...',
      // pytest's verbose progress line names the nodeid; it is what tells us
      // where run setup stopped and the first test began.
      `${PASSED.nodeid} PASSED [  5%]`,
      'first test output',
      marker(PASSED),
      'second test output',
      marker(FAILED),
    ]);

    expect(blocks.map((b) => b.title)).toEqual([
      'Run setup',
      'test_bootstrap_creates_root_tenant',
      'test_create_tenant_rejects_unknown_field',
    ]);
    expect(blocks.map((b) => b.status)).toEqual(['setup', 'pass', 'fail']);
    // The output preceding a marker belongs to that test, not to setup.
    expect(blocks[1].lines.map((l) => l.raw)).toContain('first test output');
    expect(blocks[0].lines.map((l) => l.raw)).not.toContain('first test output');
  });

  it('maps pytest outcomes, and treats an unreadable one as a failure', () => {
    const blocks = buildBlocks([marker(SKIPPED), marker({ ...PASSED, outcome: 'weird' })]);
    expect(blocks.map((b) => b.status)).toEqual(['skip', 'fail']);
  });

  it('drops the empty TEST_FILE sections the entrypoint emits up front', () => {
    const blocks = buildBlocks([
      '=== TEST_FILE: tests/a.py ===',
      '=== TEST_FILE: tests/b.py ===',
      'output for a test',
      marker(PASSED),
    ]);
    expect(blocks.map((b) => b.title)).toEqual(['test_bootstrap_creates_root_tenant']);
  });

  it('renders a malformed marker as an ordinary line instead of throwing', () => {
    const blocks = buildBlocks(['=== TEST_CASE: not-valid-base64!! ===']);
    expect(blocks).toHaveLength(1);
    expect(blocks[0].title).toBe('Run setup');
  });

  // The regression that survived the first fix: qa-runs multiplexes the pod's
  // containers and tags every line with its name, so the markers never arrive
  // at column 1. Prefix taken verbatim from the live stream for run
  // authentication-1.
  it('matches markers even though qa-runs prefixes every line with the container', () => {
    const P = '[repo-d4addf78-9b4d-49d8-940d-b1aef6d27781] ';
    const blocks = buildBlocks([
      `${P}runner: image=qa-platform-pytest-runner`,
      `${P}${PASSED.nodeid} PASSED [  5%]`,
      `${P}first test output`,
      `${P}${marker(PASSED)}`,
      `${P}second test output`,
      `${P}${marker(FAILED)}`,
    ]);
    expect(blocks.map((b) => b.title)).toEqual([
      'Run setup',
      'test_bootstrap_creates_root_tenant',
      'test_create_tenant_rejects_unknown_field',
    ]);
    expect(blocks.map((b) => b.status)).toEqual(['setup', 'pass', 'fail']);
    // The prefix stays in the rendered line: which container spoke is real
    // information when reading a failure.
    expect(blocks[1].lines.some((l) => l.raw.startsWith(P))).toBe(true);
  });

  it('still understands the source system’s TEST_FILE/TEST_RESULT pair', () => {
    const blocks = buildBlocks([
      '=== TEST_FILE: tests/infra/test_vpadm_install.py ===',
      'some output',
      '=== TEST_RESULT: tests/infra/test_vpadm_install.py PASSED ===',
    ]);
    expect(blocks.map((b) => b.title)).toEqual(['tests/infra/test_vpadm_install.py']);
    expect(blocks[0].status).toBe('pass');
  });
});

// End-to-end proof against the real thing: these are the 290 lines the browser
// actually received from the SSE stream for run authentication-1, captured
// verbatim off the live cluster. Before the container-prefix fix this produced
// ONE block ("Run setup", 290 lines) -- which is exactly what the screenshot
// that reported the bug showed.
import realLines from './__fixtures__/authentication-1.lines.json';

describe('buildBlocks against a captured live run', () => {
  it('splits authentication-1 into one section per test', () => {
    const blocks = buildBlocks(realLines as string[]);

    // 30 markers in the stream; every one must close a block.
    const resolved = blocks.filter(
      (b) => b.status === 'pass' || b.status === 'fail' || b.status === 'skip'
    );
    expect(resolved).toHaveLength(30);

    // Real test names, not "Run setup".
    expect(blocks.map((b) => b.title)).toContain('test_valid_third_party_jwt_is_accepted');
    expect(blocks.map((b) => b.title)).toContain('test_vpctl_public_client_accepts_device_authorization');

    // The one FAILED test in the run reports as a failure, not a pass.
    const failed = blocks.find((b) => b.title === 'test_vpctl_public_client_accepts_device_authorization');
    expect(failed?.status).toBe('fail');

    // The regression itself: no single block may swallow the whole run.
    expect(blocks.every((b) => b.lines.length < realLines.length)).toBe(true);
  });
});
