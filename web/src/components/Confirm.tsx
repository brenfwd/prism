import { useState, type ReactNode } from "react";
import { createStore, useStore } from "../lib/store";
import { Button, Input, Modal } from "./ui";
import { Icon } from "./icons";

interface Request {
  title: ReactNode;
  body?: ReactNode;
  confirmLabel?: string;
  danger?: boolean;
  /** The user must type this to enable the confirm button. */
  typeToConfirm?: string;
  resolve: (ok: boolean) => void;
}

const pending = createStore<Request | null>(null);

/** Asks the user to confirm; resolves `true` when they do. */
export function confirm(r: Omit<Request, "resolve">): Promise<boolean> {
  return new Promise((resolve) => pending.set({ ...r, resolve }));
}

export function ConfirmHost() {
  const r = useStore(pending);
  const [typed, setTyped] = useState("");
  if (!r) return null;
  const done = (ok: boolean) => {
    pending.set(null);
    setTyped("");
    r.resolve(ok);
  };
  const ready = !r.typeToConfirm || typed.trim() === r.typeToConfirm;
  return (
    <Modal
      size="sm"
      title={r.title}
      onClose={() => done(false)}
      icon={
        r.danger ? (
          <div className="grid size-9 shrink-0 place-items-center rounded-full bg-red-50 text-red-600 dark:bg-red-500/10 dark:text-red-400">
            <Icon name="alert" className="size-[18px]" />
          </div>
        ) : undefined
      }
      footer={
        <>
          <Button variant="ghost" onClick={() => done(false)}>Cancel</Button>
          <Button
            variant={r.danger ? "danger" : "primary"}
            disabled={!ready}
            onClick={() => done(true)}
            data-autofocus={r.typeToConfirm ? undefined : true}
          >
            {r.confirmLabel ?? "Confirm"}
          </Button>
        </>
      }
    >
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (ready) done(true);
        }}
        className="space-y-3 text-sm text-zinc-600 dark:text-zinc-300"
      >
        {r.body}
        {r.typeToConfirm && (
          <label className="block">
            <span className="mb-1.5 block text-[13px]">
              Type <span className="rounded bg-zinc-100 px-1 py-0.5 font-mono text-[12px] font-semibold text-zinc-900 dark:bg-zinc-800 dark:text-zinc-100">{r.typeToConfirm}</span> to confirm
            </span>
            <Input autoFocus data-autofocus value={typed} onChange={(e) => setTyped(e.target.value)} autoComplete="off" spellCheck={false} aria-label="Confirmation" />
          </label>
        )}
      </form>
    </Modal>
  );
}
