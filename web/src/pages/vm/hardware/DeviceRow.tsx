import type { ReactNode } from "react";
import { Icon, type IconName } from "../../../components/icons";

/** One row in a device card: icon, title with optional badge, a sub line and an action area. */
export function DeviceRow({ icon, title, sub, badge, actions }: { icon: IconName; title: ReactNode; sub: ReactNode; badge?: ReactNode; actions: ReactNode }) {
  return (
    <li className="flex flex-wrap items-center gap-x-3 gap-y-2 px-5 py-3">
      <span className="grid size-9 shrink-0 place-items-center rounded-lg bg-zinc-100 text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400">
        <Icon name={icon} className="size-[18px]" />
      </span>
      <div className="min-w-0 flex-1 basis-40">
        <div className="flex items-center gap-2">
          <span className="truncate text-sm font-medium">{title}</span>
          {badge}
        </div>
        <div className="truncate text-xs text-zinc-500">{sub}</div>
      </div>
      <div className="ml-auto">{actions}</div>
    </li>
  );
}
