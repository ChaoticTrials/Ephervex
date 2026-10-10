import {execFileSync} from 'node:child_process';
import {mkdir, readdir, readFile, rm, writeFile} from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath} from 'node:url';

const web = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repository = path.resolve(web, '..');
const destination = path.join(web, 'docs/reference/cli');
const mode = process.argv[2];

if (mode !== 'sync' && mode !== 'check') {
  throw new Error('Use cli-reference.mjs sync or cli-reference.mjs check.');
}

const commandEnvironment = {
  ...process.env,
  COLUMNS: '100',
  LC_ALL: 'C',
  NO_COLOR: '1',
};

function run(program, args, options = {}) {
  return execFileSync(program, args, {
    cwd: repository,
    encoding: 'utf8',
    env: commandEnvironment,
    maxBuffer: 16 * 1024 * 1024,
    ...options,
  }).replaceAll('\r\n', '\n').replace(/[ \t]+$/gm, '').trimEnd() + '\n';
}

if (!process.env.EPHERVEX_CLI_BIN) {
  execFileSync('cargo', ['build', '--quiet', '--package', 'ephervex-cli'], {
    cwd: repository,
    env: commandEnvironment,
    stdio: 'inherit',
  });
}

const metadata = process.env.EPHERVEX_CLI_BIN
  ? null
  : JSON.parse(run('cargo', ['metadata', '--no-deps', '--format-version', '1']));
const binary = process.env.EPHERVEX_CLI_BIN
  ?? path.join(metadata.target_directory, 'debug', 'ephervex');

function help(args) {
  return run(binary, args);
}

function subcommands(output) {
  const section = output.match(/^Commands:\n((?:  [^\n]*\n)+)/m)?.[1];
  if (!section) return [];
  return [...section.matchAll(/^  ([a-z][a-z0-9-]*) {2,}/gm)]
    .map((match) => match[1])
    .filter((name) => name !== 'help');
}

function commandHelp(parts, seen) {
  const key = parts.join(' ');
  if (seen.has(key)) throw new Error(`Repeated CLI command: ${key}`);
  seen.add(key);
  if (parts.length > 6) throw new Error(`Unexpected CLI command depth: ${key}`);
  const output = help(['help', ...parts]);
  return [{parts, output}, ...subcommands(output).flatMap((name) => commandHelp([...parts, name], seen))];
}

const rootHelp = help(['--help']);
const commands = subcommands(rootHelp);
if (commands.length === 0) throw new Error('No top-level CLI commands found.');

const expected = new Map();
expected.set('index.md', [
  '---',
  'title: "CLI Reference"',
  'sidebar_position: 1',
  '---',
  '',
  '# CLI Reference',
  '',
  'Use the task guides for workflows. These pages contain the CLI help for this build of Ephervex.',
  'Run `ephervex help COMMAND` for the same information in your terminal.',
  '',
  '```text',
  rootHelp.trimEnd(),
  '```',
  '',
].join('\n'));

const seen = new Set();
commands.forEach((name, index) => {
  const sections = commandHelp([name], seen).map(({parts, output}) => [
    `${'#'.repeat(Math.min(parts.length, 6))} \`ephervex ${parts.join(' ')}\``,
    '',
    '```text',
    output.trimEnd(),
    '```',
    '',
  ].join('\n'));
  expected.set(`${name}.md`, [
    '---',
    `title: "ephervex ${name}"`,
    `sidebar_label: "${name}"`,
    `sidebar_position: ${index + 2}`,
    '---',
    '',
    sections.join('\n'),
  ].join('\n'));
});

await mkdir(destination, {recursive: true});
const existing = (await readdir(destination)).filter((name) => name.endsWith('.md'));
const stale = [];

for (const [name, content] of expected) {
  const current = await readFile(path.join(destination, name), 'utf8').catch((error) => {
    if (error.code === 'ENOENT') return null;
    throw error;
  });
  if (current !== content) {
    stale.push(name);
    if (mode === 'sync') await writeFile(path.join(destination, name), content);
  }
}

for (const name of existing) {
  if (expected.has(name)) continue;
  stale.push(name);
  if (mode === 'sync') await rm(path.join(destination, name));
}

if (mode === 'check' && stale.length > 0) {
  throw new Error(`CLI reference is stale: ${stale.join(', ')}. Run npm run cli:reference:sync.`);
}

console.log(`${mode === 'sync' ? 'Synchronized' : 'Verified'} ${commands.length} CLI command pages.`);
