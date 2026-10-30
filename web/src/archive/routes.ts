export const archiveHomeUrl = '/road-to-v1/';

/** Build-time Docusaurus route path. Canonical URL helpers below remain prefix-free. */
export function archiveRoutePath(permalink: string, baseUrl: string): string {
  if (!baseUrl.startsWith('/') || !baseUrl.endsWith('/'))
    throw new Error(`Invalid baseUrl: ${baseUrl}`);
  return `${baseUrl.slice(0, -1)}${permalink}`;
}

function numberPath(number: number): string {
  if (!Number.isSafeInteger(number) || number < 1)
    throw new Error(`Invalid archive number: ${number}`);
  return String(number);
}

export function archiveIssueUrl(number: number): string {
  return `/road-to-v1/forgejo/issues/${numberPath(number)}/`;
}

export function archivePullUrl(number: number): string {
  return `/road-to-v1/forgejo/pulls/${numberPath(number)}/`;
}
