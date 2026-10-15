import { useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Icon } from "./icons";
import { cx } from "./ui";

function hasFiles(e: DragEvent | React.DragEvent): boolean {
  return [...(e.dataTransfer?.types ?? [])].includes("Files");
}

/** Accepts files dropped anywhere on the window while mounted. */
export function WindowDrop({ onFiles, title, hint }: { onFiles: (files: File[]) => void; title: ReactNode; hint?: ReactNode }) {
  const [over, setOver] = useState(false);
  const depth = useRef(0);
  const cb = useRef(onFiles);
  cb.current = onFiles;
  useEffect(() => {
    const enter = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      depth.current++;
      setOver(true);
    };
    const overFn = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    };
    const leave = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      depth.current = Math.max(0, depth.current - 1);
      if (!depth.current) setOver(false);
    };
    const drop = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      depth.current = 0;
      setOver(false);
      const files = [...(e.dataTransfer?.files ?? [])];
      if (files.length) cb.current(files);
    };
    window.addEventListener("dragenter", enter);
    window.addEventListener("dragover", overFn);
    window.addEventListener("dragleave", leave);
    window.addEventListener("drop", drop);
    return () => {
      window.removeEventListener("dragenter", enter);
      window.removeEventListener("dragover", overFn);
      window.removeEventListener("dragleave", leave);
      window.removeEventListener("drop", drop);
    };
  }, []);
  if (!over) return null;
  return createPortal(
    <div className="pointer-events-none fixed inset-0 z-[70] grid place-items-center bg-indigo-600/10 p-6 backdrop-blur-[2px] animate-fade dark:bg-indigo-400/10">
      <div className="flex h-full w-full flex-col items-center justify-center rounded-3xl border-2 border-dashed border-indigo-500/70 bg-white/70 text-center dark:bg-zinc-900/70">
        <div className="mb-4 grid size-16 place-items-center rounded-2xl bg-indigo-600 text-white shadow-lg shadow-indigo-600/30">
          <Icon name="upload" className="size-8" />
        </div>
        <p className="text-lg font-semibold">{title}</p>
        {hint && <p className="mt-1 text-sm text-zinc-500">{hint}</p>}
      </div>
    </div>,
    document.body,
  );
}

/** An inline drop target with a browse button. */
export function DropBox({ onFiles, accept, children, className }: {
  onFiles: (files: File[]) => void;
  accept?: string;
  children: ReactNode;
  className?: string;
}) {
  const [over, setOver] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  return (
    <div
      onDragOver={(e) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        e.stopPropagation();
        setOver(true);
      }}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        e.stopPropagation();
        setOver(false);
        onFiles([...e.dataTransfer.files]);
      }}
      className={cx(
        "rounded-xl border-2 border-dashed p-5 text-center transition-colors",
        over ? "border-indigo-500 bg-indigo-50/60 dark:bg-indigo-500/10" : "border-zinc-300 dark:border-zinc-700",
        className,
      )}
    >
      <input ref={input} type="file" className="hidden" accept={accept} onChange={(e) => (onFiles([...(e.target.files ?? [])]), (e.target.value = ""))} />
      {children}
      <button type="button" onClick={() => input.current?.click()} className="mt-2 text-sm font-medium text-indigo-600 hover:underline dark:text-indigo-400">
        Browse files…
      </button>
    </div>
  );
}
