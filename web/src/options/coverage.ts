/** What the Minecraft options dataset covers, reduced to what the page shows. */

export type OptionsDataset = {
  format_version: number;
  revision: number;
  canonical_minecraft_version: string;
  setting_registry: {
    id: string;
    key: string;
    type: string;
    category: string | null;
    manageable: boolean;
  }[];
  current_canonical_setting_ids: string[];
  schemas: { id: string; supported: Record<string, number>; unsupported: string[] }[];
  releases: { minecraft_version: string; schema: string; data_version: number | null }[];
};

export type ReleaseCoverage = {
  version: string;
  dataVersion: number | null;
  settings: number;
};

export type SettingCoverage = {
  id: string;
  key: string;
  type: string;
  manageable: boolean;
  firstRelease: string | null;
  lastRelease: string | null;
  current: boolean;
};

export type Coverage = {
  revision: number;
  formatVersion: number;
  canonicalVersion: string;
  releases: ReleaseCoverage[];
  categories: { name: string; settings: SettingCoverage[] }[];
};

const uncategorized = 'Other';

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** Checks the shape this page reads. The application's own validation is stricter. */
export function parseOptionsDataset(raw: unknown): OptionsDataset {
  if (!isRecord(raw)) throw new Error('The options dataset is not an object');
  if (raw.format_version !== 1)
    throw new Error(`Unsupported options dataset format ${String(raw.format_version)}`);
  if (!Number.isInteger(raw.revision)) throw new Error('The options dataset has no revision');
  if (typeof raw.canonical_minecraft_version !== 'string')
    throw new Error('The options dataset names no canonical release');
  for (const field of ['setting_registry', 'current_canonical_setting_ids', 'schemas', 'releases'])
    if (!Array.isArray(raw[field])) throw new Error(`The options dataset has no ${field}`);
  const dataset = raw as unknown as OptionsDataset;
  if (dataset.releases.length === 0) throw new Error('The options dataset lists no releases');
  const schemas = new Set(dataset.schemas.map((schema) => schema.id));
  const seen = new Set<string>();
  for (const release of dataset.releases) {
    if (!schemas.has(release.schema))
      throw new Error(`Release ${release.minecraft_version} names an unknown schema`);
    if (seen.has(release.minecraft_version))
      throw new Error(`Release ${release.minecraft_version} is listed twice`);
    seen.add(release.minecraft_version);
  }
  return dataset;
}

export function buildCoverage(dataset: OptionsDataset): Coverage {
  const schemas = new Map(dataset.schemas.map((schema) => [schema.id, schema]));
  const current = new Set(dataset.current_canonical_setting_ids);
  const supportedIn = new Map<string, string[]>();
  const releases = dataset.releases.map((release): ReleaseCoverage => {
    const supported = Object.keys(schemas.get(release.schema)?.supported ?? {});
    for (const id of supported) {
      const versions = supportedIn.get(id) ?? [];
      versions.push(release.minecraft_version);
      supportedIn.set(id, versions);
    }
    return {
      version: release.minecraft_version,
      dataVersion: release.data_version,
      settings: supported.length,
    };
  });
  const byCategory = new Map<string, SettingCoverage[]>();
  for (const setting of dataset.setting_registry) {
    const versions = supportedIn.get(setting.id) ?? [];
    const entry: SettingCoverage = {
      id: setting.id,
      key: setting.key,
      type: setting.type,
      manageable: setting.manageable,
      firstRelease: versions[0] ?? null,
      lastRelease: versions[versions.length - 1] ?? null,
      current: current.has(setting.id),
    };
    const category = setting.category ?? uncategorized;
    byCategory.set(category, [...(byCategory.get(category) ?? []), entry]);
  }
  const categories = [...byCategory.entries()]
    .map(([name, settings]) => ({
      name,
      settings: settings.sort((a, b) => a.key.localeCompare(b.key)),
    }))
    .sort((a, b) =>
      a.name === uncategorized ? 1 : b.name === uncategorized ? -1 : a.name.localeCompare(b.name),
    );
  return {
    revision: dataset.revision,
    formatVersion: dataset.format_version,
    canonicalVersion: dataset.canonical_minecraft_version,
    releases,
    categories,
  };
}
