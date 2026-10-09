import {createHash} from 'node:crypto';
import {readFile, readdir, mkdir, rename, rm, writeFile} from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import sharp from 'sharp';

const site = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const source = path.resolve(site, '../tools/auto-screenshot-tool/screenshots');
const assets = path.join(site, 'static/img/ephervex');
const manifestFile = path.join(site, 'src/components/screenshot-manifest.json');
const review = path.join(source, 'review');
const encoder = 'sharp-webp-lossless-effort-6';
const defaultSizes = ['1920x1080'];
const filenamePattern = /^(light|dark)-(\d+)x(\d+)\.png$/;
const safeName = /^[a-z][a-z0-9-]*$/;

function fail(message) {
  throw new Error(message);
}

function digest(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function captureKey(file) {
  return file.region === 'full' ? file.state : `${file.state}/${file.region}`;
}

function assetPath(file) {
  const directory = captureKey(file);
  return `${directory}/${file.theme}-${file.window}.webp`;
}

async function readManifest() {
  try {
    return JSON.parse(await readFile(manifestFile, 'utf8'));
  } catch (error) {
    if (error.code === 'ENOENT') return null;
    throw error;
  }
}

async function pngFiles() {
  const result = [];
  const states = await readdir(source, {withFileTypes: true});
  for (const state of states) {
    if (!state.isDirectory() || state.name === 'review') continue;
    if (!safeName.test(state.name)) fail(`Invalid screenshot state: ${state.name}`);
    const stateDirectory = path.join(source, state.name);
    const entries = await readdir(stateDirectory, {withFileTypes: true});
    for (const entry of entries) {
      if (entry.isFile()) {
        const match = filenamePattern.exec(entry.name);
        if (!match) fail(`Unexpected screenshot file: ${entry.name}`);
        result.push({
          state: state.name,
          region: 'full',
          theme: match[1],
          window: `${match[2]}x${match[3]}`,
          path: path.join(stateDirectory, entry.name),
        });
        continue;
      }
      if (!entry.isDirectory() || !safeName.test(entry.name) || entry.name === 'full') {
        fail(`Invalid screenshot crop directory: ${entry.name}. Regenerate the screenshot matrix.`);
      }
      const directory = path.join(stateDirectory, entry.name);
      for (const file of await readdir(directory, {withFileTypes: true})) {
        const match = filenamePattern.exec(file.name);
        if (!file.isFile() || !match) fail(`Unexpected screenshot file: ${file.name}`);
        result.push({
          state: state.name,
          region: entry.name,
          theme: match[1],
          window: `${match[2]}x${match[3]}`,
          path: path.join(directory, file.name),
        });
      }
    }
  }
  if (result.length === 0) fail(`No screenshots found in ${source}. Run the Rust generator first.`);
  return result.sort((a, b) => a.path.localeCompare(b.path));
}

function selectedSizes(args) {
  const sizes = [];
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] !== '--size' || !args[index + 1]) {
      fail('Use --size WIDTHxHEIGHT, repeated for multiple output sizes.');
    }
    const size = args[++index];
    if (!/^\d+x\d+$/.test(size)) fail(`Invalid size: ${size}`);
    sizes.push(size);
  }
  return [...new Set(sizes.length ? sizes : defaultSizes)];
}

function requiredFiles(files, sizes) {
  const groups = new Map();
  for (const file of files) {
    const key = captureKey(file);
    if (!groups.has(key)) groups.set(key, new Map());
    groups.get(key).set(`${file.theme}-${file.window}`, file);
  }
  const selected = [];
  for (const [key, group] of groups) {
    for (const size of sizes) {
      for (const theme of ['light', 'dark']) {
        const file = group.get(`${theme}-${size}`);
        if (!file) fail(`Missing ${theme}-${size}.png for ${key}. Regenerate the screenshot matrix.`);
        selected.push(file);
      }
    }
  }
  return selected;
}

async function sync(args) {
  const sizes = selectedSizes(args);
  const files = requiredFiles(await pngFiles(), sizes);
  const previous = await readManifest();
  const captures = {};
  let sourceBytes = 0;
  let outputBytes = 0;
  let encoded = 0;
  for (const file of files) {
    const input = await readFile(file.path);
    const sourceSha256 = digest(input);
    const sourceSize = input.length;
    const relative = assetPath(file);
    const target = path.join(assets, relative);
    const old = previous?.captures?.[captureKey(file)]?.[file.theme]
      ?.find((variant) => variant.window === file.window);
    let outputSha256;
    let bytes;
    if (previous?.encoder === encoder && old?.sourceSha256 === sourceSha256 && old?.file === relative) {
      try {
        const output = await readFile(target);
        if (digest(output) === old.outputSha256) {
          outputSha256 = old.outputSha256;
          bytes = output.length;
        }
      } catch (error) {
        if (error.code !== 'ENOENT') throw error;
      }
    }
    if (!outputSha256) {
      await mkdir(path.dirname(target), {recursive: true});
      const temporary = `${target}.tmp`;
      try {
        await sharp(input).webp({lossless: true, effort: 6}).toFile(temporary);
        await rename(temporary, target);
      } finally {
        await rm(temporary, {force: true});
      }
      const output = await readFile(target);
      outputSha256 = digest(output);
      bytes = output.length;
      encoded += 1;
    }
    const metadata = await sharp(input).metadata();
    const [windowWidth, windowHeight] = file.window.split('x').map(Number);
    if (file.region === 'full' && (metadata.width !== windowWidth || metadata.height !== windowHeight)) {
      fail(`Incorrect full-window dimensions in ${file.path}`);
    }
    const key = captureKey(file);
    captures[key] ??= {light: [], dark: []};
    captures[key][file.theme].push({
      window: file.window,
      width: metadata.width,
      height: metadata.height,
      file: relative,
      sourceSha256,
      outputSha256,
      sourceBytes: sourceSize,
      bytes,
    });
    sourceBytes += sourceSize;
    outputBytes += bytes;
  }
  for (const pair of Object.values(captures)) {
    pair.light.sort((a, b) => a.width - b.width);
    pair.dark.sort((a, b) => a.width - b.width);
  }
  const manifest = {version: 1, encoder, captures};
  await mkdir(path.dirname(manifestFile), {recursive: true});
  const temporary = `${manifestFile}.tmp`;
  try {
    await writeFile(temporary, JSON.stringify(manifest) + '\n');
    await rename(temporary, manifestFile);
  } finally {
    await rm(temporary, {force: true});
  }
  await removeStaleAssets(new Set(files.map(assetPath)));
  console.log(`${files.length} WebP files (${encoded} encoded, ${files.length - encoded} unchanged)`);
  console.log(`PNG ${formatBytes(sourceBytes)} -> WebP ${formatBytes(outputBytes)} (${Math.round(100 * outputBytes / sourceBytes)}%)`);
}

async function removeStaleAssets(expected) {
  async function visit(directory, prefix) {
    for (const entry of await readdir(directory, {withFileTypes: true})) {
      const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
      const target = path.join(directory, entry.name);
      if (entry.isDirectory()) {
        await visit(target, relative);
        if ((await readdir(target)).length === 0) await rm(target, {recursive: true});
      } else if (entry.isFile() && entry.name.endsWith('.webp') && !expected.has(relative)) {
        await rm(target);
      }
    }
  }
  try {
    await visit(assets, '');
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
}

async function check() {
  const manifest = await readManifest();
  if (!manifest || manifest.version !== 1 || !Object.keys(manifest.captures ?? {}).length) {
    fail('Screenshot assets are missing. Run npm run screenshots:sync before building.');
  }
  let count = 0;
  for (const [key, pair] of Object.entries(manifest.captures)) {
    const light = pair.light.map((variant) => variant.window).sort();
    const dark = pair.dark.map((variant) => variant.window).sort();
    if (!light.length || JSON.stringify(light) !== JSON.stringify(dark)) {
      fail(`Theme variants differ for ${key}`);
    }
    for (const variant of [...pair.light, ...pair.dark]) {
      const expectedPrefix = `${key}/`;
      if (!variant.file.startsWith(expectedPrefix) || !/^[a-z0-9/-]+\.webp$/.test(variant.file)) {
        fail(`Invalid screenshot asset path: ${variant.file}`);
      }
      const target = path.join(assets, variant.file);
      let output;
      try {
        output = await readFile(target);
      } catch (error) {
        if (error.code === 'ENOENT') fail(`Missing screenshot asset: ${target}. Run npm run screenshots:sync.`);
        throw error;
      }
      const metadata = await sharp(output).metadata();
      if (metadata.format !== 'webp' || metadata.width !== variant.width ||
          metadata.height !== variant.height || digest(output) !== variant.outputSha256) {
        fail(`Damaged screenshot asset: ${target}. Run npm run screenshots:sync.`);
      }
      count += 1;
    }
  }
  console.log(`Verified ${count} screenshot WebP files.`);
}

function formatBytes(value) {
  return `${(value / 1024 / 1024).toFixed(2)} MiB`;
}

function escapeSvg(value) {
  return value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
}

async function contactSheet() {
  const files = await pngFiles();
  await mkdir(review, {recursive: true});
  const sizes = [...new Set(files.map((file) => file.window))].sort();
  for (const size of sizes) {
    for (const kind of ['full', 'crops']) {
      const chosen = files.filter((file) => file.window === size &&
        (kind === 'full' ? file.region === 'full' : file.region !== 'full'));
      if (!chosen.length) continue;
      const rows = [...new Set(chosen.map((file) => `${file.state}/${file.region}`))].sort();
      const cellWidth = 1200;
      const imageHeight = kind === 'full' ? 675 : Math.ceil(Math.max(
        ...await Promise.all(chosen.map(async (file) => {
          const metadata = await sharp(file.path).metadata();
          return metadata.height * Math.min(1, cellWidth / metadata.width);
        })),
      ));
      const cellHeight = imageHeight + 48;
      const gap = 20;
      const width = cellWidth * 2 + gap * 3;
      const height = rows.length * cellHeight + (rows.length + 1) * gap;
      const overlays = [];
      for (const [rowIndex, row] of rows.entries()) {
        for (const [column, theme] of ['light', 'dark'].entries()) {
          const file = chosen.find((item) => `${item.state}/${item.region}` === row && item.theme === theme);
          if (!file) fail(`Missing ${theme} review capture for ${row} at ${size}`);
          const left = gap + column * (cellWidth + gap);
          const top = gap + rowIndex * (cellHeight + gap);
          const label = escapeSvg(`${row} / ${theme} / ${size}`);
          const caption = Buffer.from(`<svg width="${cellWidth}" height="40"><text x="12" y="28" font-family="sans-serif" font-size="23" fill="#171a21">${label}</text></svg>`);
          overlays.push({input: caption, left, top});
          const thumbnail = kind === 'full'
            ? await sharp(file.path).resize(cellWidth, imageHeight, {fit: 'contain'}).png().toBuffer()
            : await sharp(file.path).resize({width: cellWidth, withoutEnlargement: true}).png().toBuffer();
          overlays.push({input: thumbnail, left, top: top + 48});
        }
      }
      const target = path.join(review, `${kind}-${size}.png`);
      await sharp({create: {width, height, channels: 4, background: '#e7e9ee'}})
        .composite(overlays).png().toFile(target);
      console.log(target);
    }
  }
}

const [command, ...args] = process.argv.slice(2);
try {
  if (command === 'sync') await sync(args);
  else if (command === 'check' && !args.length) await check();
  else if (command === 'contact-sheet' && !args.length) await contactSheet();
  else fail('Use sync [--size WIDTHxHEIGHT], check, or contact-sheet.');
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
