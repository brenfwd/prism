import { Icon, type IconName } from "../../components/icons";
import { Badge, Card, cx } from "../../components/ui";
import type { Source } from "./model";
import { Step } from "./parts";
import { SourcePicker } from "./SourcePicker";
import type { NewVmForm } from "./useNewVmForm";

const sources: { id: Source; title: string; body: string; icon: IconName; badge?: string }[] = [
  { id: "image", title: "Cloud image", body: "Boots in seconds, set up for you with your SSH key.", icon: "cloud", badge: "Fastest" },
  { id: "iso", title: "Install from ISO", body: "Run an OS installer on a fresh, empty disk.", icon: "disc" },
  { id: "blank", title: "Blank machine", body: "An empty disk. Network boot or attach media later.", icon: "disk" },
];

/** Step 1: what the machine boots from, and which file. */
export function SourceStep({ form }: { form: NewVmForm }) {
  const { f, set } = form;
  return (
    <Card title={<Step n={1}>Start from</Step>}>
      <div role="radiogroup" aria-label="Start from" className="grid gap-3 sm:grid-cols-3">
        {sources.map((s) => (
          <button
            key={s.id}
            type="button"
            role="radio"
            aria-checked={f.source === s.id}
            onClick={() => set({ source: s.id })}
            className={cx(
              "relative flex flex-col items-start rounded-xl p-3.5 text-left ring-1 transition-all ring-inset",
              f.source === s.id
                ? "bg-indigo-50/70 ring-2 ring-indigo-500 dark:bg-indigo-500/10 dark:ring-indigo-400"
                : "ring-zinc-200 hover:bg-zinc-50 hover:ring-zinc-300 dark:ring-zinc-700/80 dark:hover:bg-white/[0.03]",
            )}
          >
            <span className={cx("mb-2.5 grid size-8 place-items-center rounded-lg", f.source === s.id ? "bg-indigo-600 text-white dark:bg-indigo-500" : "bg-zinc-100 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300")}>
              <Icon name={s.icon} className="size-[18px]" />
            </span>
            <span className="text-sm font-semibold">{s.title}</span>
            <span className="mt-0.5 text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">{s.body}</span>
            {s.badge && <Badge tone="indigo" className="absolute top-3 right-3">{s.badge}</Badge>}
          </button>
        ))}
      </div>
      {f.source !== "blank" && (
        <SourcePicker
          kind={f.source}
          items={form.candidates}
          loading={form.volumesLoading}
          picked={form.sourceVol}
          onPick={(v) => set({ picked: { pool: v.pool, name: v.name } })}
          defaultPool={form.pools[0]?.name}
          onUploaded={(pool, name) => set({ picked: { pool, name } })}
        />
      )}
    </Card>
  );
}
