import { archiveIssueUrl, archivePullUrl } from './routes.ts';

type TreeNode = { type: string; value?: string; url?: string; children?: TreeNode[] };

export function safeArchiveUrl(url: string): string {
  const value = url.trim();
  if (/^(https?:|mailto:)/i.test(value)) {
    try {
      const parsed = new URL(value);
      if (parsed.hostname.endsWith('.lan') || parsed.hostname === 'forgejo.lan') return '';
      return value;
    } catch {
      return '';
    }
  }
  // Forgejo-relative paths often point to attachments omitted from schema v1.
  // Keep only verified archive routes and local heading fragments clickable.
  return value.startsWith('/road-to-v1/') || /^#[A-Za-z0-9_-]+$/.test(value) ? value : '';
}

export function linkArchiveReferences(issues: number[], pulls: number[]) {
  const issueSet = new Set(issues);
  const pullSet = new Set(pulls);
  return () => (tree: TreeNode) => {
    const walk = (node: TreeNode) => {
      if (
        !node.children ||
        ['link', 'linkReference', 'code', 'inlineCode', 'image'].includes(node.type)
      )
        return;
      node.children = node.children.flatMap((child) => {
        if (child.type !== 'text' || !child.value) {
          walk(child);
          return [child];
        }
        const result: TreeNode[] = [];
        const pattern = /(^|[^\w/])#([1-9]\d*)\b/g;
        let start = 0;
        for (const match of child.value.matchAll(pattern)) {
          const number = Number(match[2]);
          const url = issueSet.has(number)
            ? archiveIssueUrl(number)
            : pullSet.has(number)
              ? archivePullUrl(number)
              : null;
          if (!url) continue;
          const index = match.index + match[1].length;
          if (index > start) result.push({ type: 'text', value: child.value.slice(start, index) });
          result.push({ type: 'link', url, children: [{ type: 'text', value: `#${number}` }] });
          start = index + match[2].length + 1;
        }
        if (start < child.value.length)
          result.push({ type: 'text', value: child.value.slice(start) });
        return result.length ? result : [child];
      });
    };
    walk(tree);
  };
}
