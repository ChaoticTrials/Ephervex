import React from 'react';

export default function ArchiveLabel({
  name,
  color,
}: {
  name: string;
  color?: string;
}): React.JSX.Element {
  const safeColor = color && /^#[a-f\d]{6}$/i.test(color) ? color : undefined;
  return (
    <span
      className="archive-label"
      style={safeColor ? { '--archive-label-color': safeColor } as React.CSSProperties : undefined}
    >
      {name}
    </span>
  );
}
