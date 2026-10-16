import { Icon } from "../../components/icons";
import { Callout, Card, Field, Input } from "../../components/ui";
import { bytes, memLabel } from "../../lib/format";
import { hostnameOf, memExceeds, presets, type PresetId } from "./model";
import { PresetCard, Step } from "./parts";
import type { NewVmForm } from "./useNewVmForm";

/** Step 2: the machine's name, then a size card or custom numbers, with capacity warnings. */
export function SizeStep({ form }: { form: NewVmForm }) {
  const { f, set, node, avail, minDisk, cap, poolInfo } = form;
  const pick = (preset: PresetId) => set({ preset, presetTouched: true });
  return (
    <Card title={<Step n={2}>Name and size</Step>}>
      <Field
        label="Name"
        htmlFor="vm-name"
        error={form.showNameError ? form.nameProblem : null}
        hint={f.source === "image" ? <>Also used as the hostname (<span className="font-mono">{hostnameOf(f.name)}</span>).</> : "Letters, digits, dots, dashes."}
      >
        <div className="relative">
          <Input
            id="vm-name"
            value={f.name}
            aria-invalid={!!form.nameProblem && form.showNameError}
            onChange={(e) => set({ nameTouched: true, name: e.target.value.replace(/\s/g, "-") })}
            spellCheck={false}
            autoComplete="off"
            className="pr-24 font-medium"
          />
          {!form.nameProblem && <span className="absolute top-1/2 right-3 flex -translate-y-1/2 items-center gap-1 text-xs text-emerald-600 dark:text-emerald-400"><Icon name="check" className="size-3.5" />Available</span>}
        </div>
      </Field>

      <div className="mt-5">
        <div className="mb-1.5 text-[13px] font-medium text-zinc-700 dark:text-zinc-300">Size</div>
        <div role="radiogroup" aria-label="Size" className="grid grid-cols-2 gap-2.5 sm:grid-cols-4">
          {(Object.keys(presets) as Exclude<PresetId, "custom">[]).map((id) => {
            const p = presets[id];
            return (
              <PresetCard key={id} active={f.preset === id} onClick={() => pick(id)} title={p.label} warn={memExceeds(p.mem, avail) ? "More than free memory" : undefined}>
                <span className="tabular-nums">{p.vcpus} vCPU · {memLabel(p.mem)}</span>
                <span className="tabular-nums">{Math.max(p.disk, minDisk)} GiB disk</span>
              </PresetCard>
            );
          })}
          <PresetCard active={f.preset === "custom"} onClick={() => pick("custom")} title="Custom">
            <span>Your numbers</span>
            <span className="tabular-nums">{f.custom.vcpus} · {memLabel(f.custom.mem)} · {f.custom.disk} G</span>
          </PresetCard>
        </div>
        {f.preset !== "custom" && <p className="mt-2 text-xs text-zinc-500">{presets[f.preset].blurb}.</p>}
        {f.preset === "custom" && (
          <div className="mt-4 grid gap-4 sm:grid-cols-3">
            <Field label="vCPUs" htmlFor="c-cpu" hint={node ? `This host has ${node.host.cpus}.` : undefined}>
              <Input id="c-cpu" type="number" min={1} max={512} value={f.custom.vcpus} onChange={(e) => set({ custom: { ...f.custom, vcpus: Math.max(1, +e.target.value) } })} />
            </Field>
            <Field label="Memory (MiB)" htmlFor="c-mem" hint={avail != null ? `${bytes(avail)} free on the host.` : undefined}>
              <Input id="c-mem" type="number" min={128} step={256} value={f.custom.mem} onChange={(e) => set({ custom: { ...f.custom, mem: Math.max(64, +e.target.value) } })} />
            </Field>
            <Field label="Disk (GiB)" htmlFor="c-disk" hint={minDisk > 1 ? `At least ${minDisk} GiB for this image.` : undefined}>
              <Input id="c-disk" type="number" min={minDisk} value={f.custom.disk} onChange={(e) => set({ custom: { ...f.custom, disk: Math.max(1, +e.target.value) } })} />
            </Field>
          </div>
        )}
        {node?.memory_pressure?.level === "red" && (
          <Callout tone="warning" className="mt-4" title="The host is short on memory right now">
            Prism won't start new machines until the pressure eases. You can still create this one and start it later.
          </Callout>
        )}
        {cap.memOver && (
          <Callout tone="warning" className="mt-4" title={`${memLabel(form.size.mem)} is more than the ${bytes(avail!)} this host has free`}>
            The machine may fail to start, or push the host into swapping. Pick a smaller size, or stop another machine first.
          </Callout>
        )}
        {cap.memTight && (
          <Callout tone="info" className="mt-4">
            That uses most of the host's free memory. With automatic ballooning on, Prism hands unused guest memory back when the host gets tight.
          </Callout>
        )}
        {cap.diskOver && (
          <Callout tone="warning" className="mt-4" title="Bigger than the free space in this pool">
            Disks are thin, so this works until the guest actually fills it, then writes will fail. {bytes(poolInfo!.available_bytes)} is free in {form.pool}.
          </Callout>
        )}
      </div>
    </Card>
  );
}
