import { useState } from "react";
import { Icon } from "../../components/icons";
import { Badge, Button, Callout, Card, Checkbox, Field, Input, Textarea } from "../../components/ui";
import { PackageInput } from "./PackageInput";
import { Step } from "./parts";
import type { NewVmForm } from "./useNewVmForm";

/** Step 3 for cloud images: the user, password, SSH key and packages that go into cloud-init, or raw user-data. */
export function AccessStep({ form }: { form: NewVmForm }) {
  const { f, set, badKey, generated } = form;
  const [showPw, setShowPw] = useState(false);
  const [showRaw, setShowRaw] = useState(false);
  const locked = f.raw != null;
  return (
    <Card title={<Step n={3}>Sign-in</Step>} description="Prism writes a cloud-init config so the machine is ready to use on first boot.">
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Username" htmlFor="ci-user">
          <Input id="ci-user" value={f.username} onChange={(e) => set({ username: e.target.value.replace(/[^a-z0-9_-]/gi, ""), userTouched: true })} disabled={locked} autoComplete="off" spellCheck={false} />
        </Field>
        <Field label="Password" htmlFor="ci-pw" hint="Optional. Leave empty for key-only SSH." aside={f.password && <button type="button" onClick={() => setShowPw(!showPw)} className="text-indigo-600 hover:underline dark:text-indigo-400">{showPw ? "Hide" : "Show"}</button>}>
          <Input id="ci-pw" type={showPw ? "text" : "password"} value={f.password} onChange={(e) => set({ password: e.target.value })} disabled={locked} autoComplete="new-password" />
        </Field>
      </div>
      <Field
        className="mt-4"
        label={<span className="inline-flex items-center gap-1.5"><Icon name="key" className="size-3.5" />SSH public key</span>}
        htmlFor="ci-key"
        aside={form.rememberedKey && f.sshKeys === form.rememberedKey ? "Remembered from last time" : undefined}
        error={badKey ? `This doesn't look like a public key: "${badKey.slice(0, 32)}…"` : undefined}
        hint={<>Paste the contents of <span className="font-mono">~/.ssh/id_ed25519.pub</span>. One key per line.</>}
      >
        <Textarea
          id="ci-key"
          rows={3}
          value={f.sshKeys}
          onChange={(e) => set({ sshKeys: e.target.value })}
          placeholder="ssh-ed25519 AAAAC3Nza… you@laptop"
          disabled={locked}
          aria-invalid={!!badKey}
          spellCheck={false}
          className="break-all"
        />
      </Field>
      <PackageInput className="mt-4" value={f.packages} onChange={(packages) => set({ packages })} disabled={locked} />
      <div className="mt-4">
        <Checkbox checked={f.upgrade} onChange={(upgrade) => set({ upgrade })} disabled={locked}>
          Install OS updates on first boot <span className="text-zinc-500">(slower first boot)</span>
        </Checkbox>
      </div>
      {form.noLogin && (
        <Callout tone="warning" className="mt-4">
          Without an SSH key or a password you won't be able to sign in, except through the serial console if the image allows it.
        </Callout>
      )}
      <div className="mt-5 border-t border-zinc-100 pt-4 dark:border-zinc-800">
        <button type="button" onClick={() => setShowRaw(!showRaw)} className="flex items-center gap-1.5 text-[13px] font-medium text-zinc-600 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-zinc-100" aria-expanded={showRaw}>
          <Icon name={showRaw ? "chevronDown" : "chevronRight"} className="size-4" />
          <Icon name="code" className="size-4" />
          {locked ? "Custom user-data" : "Edit raw user-data"}
          {locked && <Badge tone="amber">edited</Badge>}
        </button>
        {showRaw && (
          <div className="mt-3">
            <Textarea rows={14} value={f.raw ?? generated} onChange={(e) => set({ raw: e.target.value })} spellCheck={false} aria-label="cloud-init user-data" />
            <div className="mt-2 flex items-center justify-between text-xs text-zinc-500">
              <span>{locked ? "Your edits replace the fields above." : "Edits here take over from the fields above."}</span>
              {locked && <Button size="xs" variant="ghost" icon="rotate" onClick={() => set({ raw: null })}>Back to generated</Button>}
            </div>
          </div>
        )}
      </div>
    </Card>
  );
}
