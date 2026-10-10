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

function commandSummaries(output) {
  const section = output.match(/^Commands:\n((?:  [^\n]*\n)+)/m)?.[1];
  if (!section) return [];
  return [...section.matchAll(/^  ([a-z][a-z0-9-]*) {2,}(.+)$/gm)]
    .map(([, name, description]) => ({name, description: description.trim()}))
    .filter(({name}) => name !== 'help');
}

function subcommands(output) {
  return commandSummaries(output).map(({name}) => name);
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
const summaries = commandSummaries(rootHelp);
const commands = summaries.map(({name}) => name);
if (commands.length === 0) throw new Error('No top-level CLI commands found.');
const rootHelpWithoutCommands = rootHelp
  .replace(/^Commands:\n(?:  [^\n]*\n)+/m, '')
  .replace(/\n{3,}/g, '\n\n')
  .trimEnd();

const expected = new Map();
expected.set('index.md', [
  '---',
  'title: "CLI Reference"',
  'custom_edit_url: null',
  '---',
  '',
  '# CLI Reference',
  '',
  'Use the task guides for workflows. These pages contain the CLI help for this build of Ephervex.',
  'Run `ephervex help COMMAND` for the same information in your terminal.',
  '',
  '## Commands',
  '',
  '| Command | Description |',
  '| --- | --- |',
  ...summaries.map(({name, description}) =>
    `| [\`${name}\`](./commands/${name}.md) | ${description.replaceAll('|', '\\|')} |`),
  '',
  '## Usage and global options',
  '',
  '```text',
  rootHelpWithoutCommands,
  '```',
  '',
].join('\n'));

const seen = new Set();
commands.forEach((name, index) => {
  const sections = commandHelp([name], seen).map(({parts, output}) => {
    const heading = parts.length === 1
      ? `# ephervex ${name}\n\n${summaries[index].description}\n\n## Usage and options`
      : `${'#'.repeat(Math.min(parts.length, 6))} \`ephervex ${parts.join(' ')}\``;
    return [heading, '', '```text', output.trimEnd(), '```', ''].join('\n');
  });
  expected.set(`commands/${name}.md`, [
    '---',
    `title: "ephervex ${name}"`,
    `sidebar_label: "${name}"`,
    `sidebar_position: ${index + 2}`,
    `slug: /reference/cli/${name}`,
    'custom_edit_url: null',
    '---',
    '',
    sections.join('\n'),
  ].join('\n'));
});

await mkdir(destination, {recursive: true});
const commandsDestination = path.join(destination, 'commands');
await mkdir(commandsDestination, {recursive: true});
const existing = [
  ...(await readdir(destination)).filter((name) => name.endsWith('.md')),
  ...(await readdir(commandsDestination))
    .filter((name) => name.endsWith('.md'))
    .map((name) => `commands/${name}`),
];
const stale = [];

for (const [name, content] of expected) {
  const current = await readFile(path.join(destination, name), 'utf8').catch((error) => {
    if (error.code === 'ENOENT') return null;
    throw error;
  });
  if (current !== content) {
    stale.push(name);
    if (mode === 'sync') {
      await mkdir(path.dirname(path.join(destination, name)), {recursive: true});
      await writeFile(path.join(destination, name), content);
    }
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
