import { useState } from "react";
import { Icon } from "../../components/icons";
import { Field, cx } from "../../components/ui";

/** A chip input for cloud-init packages: Enter, comma or space adds, Backspace removes the last. */
export function PackageInput({ value, onChange, disabled, className }: { value: string[]; onChange: (v: string[]) => void; disabled?: boolean; className?: string }) {
  const [draft, setDraft] = useState("");
  const add = () => {
    const parts = draft.split(/[\s,]+/).filter((p) => p && !value.includes(p));
    if (parts.length) onChange([...value, ...parts]);
    setDraft("");
  };
  return (
    <Field className={className} label="Packages" htmlFor="pkgs" hint="Installed on first boot. The guest agent lets Prism talk to the machine for clean shutdowns and status.">
      <div className={cx("flex flex-wrap items-center gap-1.5 rounded-lg px-2 py-1.5 ring-1 ring-zinc-300 ring-inset focus-within:ring-2 focus-within:ring-indigo-500 dark:ring-zinc-700", disabled && "opacity-60")}>
        {value.map((p) => (
          <span key={p} className="inline-flex items-center gap-1 rounded-md bg-zinc-100 py-0.5 pr-1 pl-2 font-mono text-xs dark:bg-zinc-800">
            {p}
            <button type="button" disabled={disabled} aria-label={`Remove ${p}`} onClick={() => onChange(value.filter((x) => x !== p))} className="rounded p-0.5 text-zinc-400 hover:bg-zinc-200 hover:text-zinc-700 dark:hover:bg-zinc-700">
              <Icon name="x" className="size-3" />
            </button>
          </span>
        ))}
        <input
          id="pkgs"
          value={draft}
          disabled={disabled}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === "," || e.key === " ") {
              e.preventDefault();
              add();
            } else if (e.key === "Backspace" && !draft && value.length) onChange(value.slice(0, -1));
          }}
          onBlur={add}
          placeholder={value.length ? "Add more…" : "e.g. htop nginx"}
          className="min-w-24 flex-1 bg-transparent py-0.5 font-mono text-xs outline-none placeholder:font-sans placeholder:text-zinc-400"
        />
      </div>
    </Field>
  );
}
