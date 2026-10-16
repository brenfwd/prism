import { Icon } from "../../components/icons";
import { Field, Input, Segmented, Select, Switch } from "../../components/ui";
import { bytes } from "../../lib/format";
import type { NewVmForm } from "./useNewVmForm";

const balloonHint = {
  auto: "Prism reclaims memory the guest isn't using when the host is under pressure, never below a quarter of its size.",
  manual: "You can resize the guest's memory live from its page.",
  off: "The guest always keeps its full memory.",
};

/** Collapsed section with firmware, CPU model, ballooning, storage pool, description and autostart. */
export function AdvancedStep({ form }: { form: NewVmForm }) {
  const { f, set, pools } = form;
  return (
    <details className="group rounded-xl bg-white ring-1 ring-zinc-200/90 dark:bg-zinc-900/70 dark:ring-zinc-800">
      <summary className="flex list-none items-center gap-2 px-5 py-4 text-[15px] font-semibold tracking-tight [&::-webkit-details-marker]:hidden">
        <Icon name="chevronRight" className="size-4 transition-transform group-open:rotate-90" />
        Advanced
        <span className="ml-1 text-[13px] font-normal text-zinc-500">firmware, CPU model, memory ballooning, storage pool</span>
      </summary>
      <div className="grid gap-5 px-5 pb-5 sm:grid-cols-2">
        <Field label="Firmware" hint={f.firmware === "uefi" ? "Needed for Secure Boot and some newer OSes." : "Classic BIOS. Works with nearly every image."}>
          <Segmented label="Firmware" value={f.firmware} onChange={(firmware) => set({ firmware })} options={[{ value: "bios", label: "BIOS" }, { value: "uefi", label: "UEFI" }]} />
        </Field>
        <Field label="CPU model" hint={f.cpuModel === "host_model" ? "The host CPU's features; can move between similar hosts." : "Exactly the host CPU: fastest, but can only move to identical hosts."}>
          <Segmented label="CPU model" value={f.cpuModel} onChange={(cpuModel) => set({ cpuModel })} options={[{ value: "host_model", label: "Host model" }, { value: "host_passthrough", label: "Passthrough" }]} />
        </Field>
        <Field label="Memory ballooning" hint={balloonHint[f.balloon]}>
          <Segmented label="Memory ballooning" value={f.balloon} onChange={(balloon) => set({ balloon })} options={[{ value: "auto", label: "Automatic" }, { value: "manual", label: "Manual" }, { value: "off", label: "Off" }]} />
        </Field>
        <Field label="Storage pool" hint={f.source === "blank" ? "Where the new disk lives." : "The disk is created next to the source image."}>
          <Select value={form.pool ?? ""} disabled={f.source !== "blank"} onChange={(e) => set({ blankPool: e.target.value })}>
            {pools.map((p) => (
              <option key={p.name} value={p.name}>{p.name} · {bytes(p.available_bytes)} free</option>
            ))}
          </Select>
        </Field>
        <Field label="Description" className="sm:col-span-2">
          <Input value={f.description} onChange={(e) => set({ description: e.target.value })} placeholder="What's it for?" />
        </Field>
        <div className="sm:col-span-2">
          <Switch checked={f.autostart} onChange={(autostart) => set({ autostart })} label="Start when the host boots" description="Prism starts it automatically after the host restarts." />
        </div>
      </div>
    </details>
  );
}
