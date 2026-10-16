import { AccessStep } from "./new-vm/AccessStep";
import { AdvancedStep } from "./new-vm/AdvancedStep";
import { NetworkStep } from "./new-vm/NetworkStep";
import { SizeStep } from "./new-vm/SizeStep";
import { SourceStep } from "./new-vm/SourceStep";
import { Summary } from "./new-vm/Summary";
import { useNewVmForm, type NewVmSearch } from "./new-vm/useNewVmForm";

export type { NewVmSearch };

/** The new-machine page: numbered steps on the left, a summary with the create button on the right. */
export function NewVmPage({ search }: { search: NewVmSearch }) {
  const form = useNewVmForm(search);
  const image = form.f.source === "image";
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        form.submit();
      }}
      className="animate-in"
    >
      <div className="mb-6">
        <h1 className="text-2xl font-semibold tracking-tight">New machine</h1>
        <p className="mt-1 text-sm text-zinc-500 dark:text-zinc-400">Pick what it boots, give it a size, and go. Everything here can be changed later.</p>
      </div>

      <div className="grid items-start gap-6 lg:grid-cols-[minmax(0,1fr)_19rem]">
        <div className="space-y-5">
          <SourceStep form={form} />
          <SizeStep form={form} />
          {image && <AccessStep form={form} />}
          <NetworkStep form={form} n={image ? 4 : 3} />
          <AdvancedStep form={form} />
        </div>
        <aside className="lg:sticky lg:top-20">
          <Summary form={form} />
        </aside>
      </div>
    </form>
  );
}
