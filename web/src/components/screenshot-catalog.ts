export type CatalogEntry = {
  state: string;
  title: string;
  /** Crops of this state, by region name. */
  regions?: Record<string, string>;
};

export type CatalogGroup = {
  id: string;
  title: string;
  entries: CatalogEntry[];
};

/** Display order and titles. States missing here are listed under "Other". */
export const catalogGroups: CatalogGroup[] = [
  {
    id: 'instances',
    title: 'Instances',
    entries: [
      {state: 'instances-overview', title: 'Instance list', regions: {'client-card': 'Instance card'}},
      {state: 'instances-empty', title: 'No instances yet'},
      {state: 'create-instance-dialog', title: 'Create an instance'},
      {state: 'instance-launching', title: 'Game starting'},
      {state: 'client-overview-linked-server', title: 'Instance with a linked server'},
      {state: 'client-settings', title: 'Per-instance Java and memory'},
      {state: 'running-page', title: 'Running games and servers'},
    ],
  },
  {
    id: 'import',
    title: 'Importing modpacks',
    entries: [
      {state: 'import-modpack-resolving', title: 'Reading a pack'},
      {state: 'import-modpack-ready', title: 'Pack ready to import'},
      {state: 'import-modpack-duplicates', title: 'Duplicate projects in a pack'},
      {state: 'import-modpack-manual-download', title: 'Manual CurseForge download', regions: {'blocked-file': 'Open in Browser'}},
      {state: 'import-modpack-credential-missing', title: 'CurseForge key missing'},
      {state: 'import-modpack-running', title: 'Import in progress'},
      {state: 'activity-import-in-progress', title: 'Activity history'},
      {state: 'file-drop-overlay', title: 'Dropping a pack onto the window'},
    ],
  },
  {
    id: 'mods',
    title: 'Mods',
    entries: [
      {state: 'client-mods', title: 'Managed mods and catalog'},
      {state: 'mod-manager', title: 'One mod: versions and dependencies'},
      {state: 'mod-impact', title: 'What turning a mod off affects'},
      {state: 'mod-install-plan', title: 'Install confirmation'},
      {state: 'mod-isolation-setup', title: 'Mod isolation: setup'},
      {state: 'mod-isolation-round', title: 'Mod isolation: a round'},
    ],
  },
  {
    id: 'servers',
    title: 'Servers',
    entries: [
      {state: 'servers-overview', title: 'Server list', regions: {'server-card': 'Server card'}},
      {state: 'server-starting', title: 'Server starting'},
      {state: 'server-running', title: 'Server running'},
      {state: 'server-console', title: 'Server console'},
      {state: 'server-properties', title: 'Server properties and EULA'},
      {state: 'server-crash-notice', title: 'Server crash notice'},
      {state: 'split-workspace', title: 'Split workspace'},
    ],
  },
  {
    id: 'troubleshooting',
    title: 'Troubleshooting',
    entries: [
      {state: 'client-logs-live', title: 'Live log'},
      {state: 'client-crash-reports', title: 'Crash reports'},
      {state: 'client-run-failure-notice', title: 'Failed run notice'},
      {state: 'diagnostics-working', title: 'Diagnosis running'},
      {state: 'client-diagnostics-result', title: 'Diagnosis result'},
      {state: 'client-diagnostics-fix-preview', title: 'Fix preview'},
      {state: 'snapshots', title: 'Snapshots'},
      {state: 'snapshot-restore-dialog', title: 'Restoring a snapshot'},
    ],
  },
  {
    id: 'accounts',
    title: 'Accounts',
    entries: [
      {state: 'accounts-settings', title: 'Accounts', regions: {'offline-profile': 'Offline account'}},
      {state: 'accounts-device-code', title: 'Microsoft sign-in prompt'},
      {state: 'toasts-success-and-error', title: 'Notifications'},
    ],
  },
  {
    id: 'settings',
    title: 'Settings',
    entries: [
      {state: 'settings-general', title: 'General'},
      {state: 'settings-minecraft-defaults', title: 'Minecraft defaults'},
      {state: 'settings-java', title: 'Java'},
      {state: 'settings-providers', title: 'Providers and APIs'},
      {state: 'settings-storage', title: 'Storage'},
      {state: 'settings-disk-usage', title: 'Disk usage'},
    ],
  },
];
