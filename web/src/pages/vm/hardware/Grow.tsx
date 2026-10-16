import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type Disk, type Vm } from "../../../api";
import { bytes, GiB } from "../../../lib/format";
import { Button, ErrorText, Field, Input, Modal } from "../../../components/ui";
import { deviceChange } from "./deviceChange";

/** Dialog that raises a disk to a larger size in whole GiB. Disks cannot shrink. */
export function Grow({ vm, disk, current, onClose }: { vm: Vm; disk: Disk; current?: number; onClose: () => void }) {
  const qc = useQueryClient();
  const cur = current ? Math.ceil(current / GiB) : 1;
  const [size, setSize] = useState(cur + 10);
  const m = useMutation({
    mutationFn: () => deviceChange(qc, `${disk.target} grown to ${size} GiB`, () => api.updateDisk(vm.id, disk.id, { size_gib: size })),
    onSuccess: (ok) => ok && onClose(),
  });
  return (
    <Modal
      title={`Grow ${disk.target}`}
      description="Disks can only grow. Afterwards, extend the partition inside the guest (cloud images do this by themselves at boot)."
      onClose={onClose}
      size="sm"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={m.isPending} disabled={size <= cur} onClick={() => m.mutate()}>Grow to {size} GiB</Button>
        </>
      }
    >
      <Field label="New size (GiB)" htmlFor="grow" hint={current ? `Currently ${bytes(current)}.` : undefined}>
        <Input id="grow" type="number" min={cur + 1} value={size} onChange={(e) => setSize(+e.target.value)} autoFocus />
      </Field>
      {m.error && <ErrorText className="mt-3" error={m.error} />}
    </Modal>
  );
}
