import { Archive, FilePlus2, Settings, Shapes, Trash2 } from "lucide-react";

import type { IconName } from "./nav";

const ICONS = {
  archive: Archive,
  upload: FilePlus2,
  taxonomy: Shapes,
  trash: Trash2,
  settings: Settings,
} as const;

export function NavIcon({ name }: { name: IconName }) {
  const Icon = ICONS[name];
  return <Icon aria-hidden="true" />;
}
