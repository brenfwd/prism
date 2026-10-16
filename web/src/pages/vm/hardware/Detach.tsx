import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type Disk, type Vm } from "../../../api";
import { Icon } from "../../../components/icons";
import { Button, Checkbox, Modal } from "../../../components/ui";
import { isOn } from "../VmPage";
import { deviceChange } from "./deviceChange";

/** Confirmation dialog for removing a disk, with a checkbox to delete its volume (on by default for disks this VM created). */
export function Detach({ vm, disk, onClose }: { vm: Vm; disk: Disk; onClose: () => void }) {
  const qc = useQueryClient();
  const owned = !!disk.volume?.startsWith(`${vm.name}-disk`);
  const [del, setDel] = useState(owned);
  const m = useMutation({
    mutationFn: () => deviceChange(qc, del ? `${disk.target} removed and deleted` : `${disk.target} detached`, () => api.removeDisk(vm.id, disk.id, del)),
    onSuccess: (ok) => ok && onClose(),
  });
  return (
    <Modal
      title={`Detach ${disk.target}?`}
      size="sm"
      onClose={onClose}
      icon={<div className="grid size-9 shrink-0 place-items-center rounded-full bg-red-50 text-red-600 dark:bg-red-500/10 dark:text-red-400"><Icon name="disk" className="size-[18px]" /></div>}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="danger" loading={m.isPending} onClick={() => m.mutate()}>{del ? "Detach and delete" : "Detach"}</Button>
        </>
      }
    >
      <div className="space-y-3 text-sm text-zinc-600 dark:text-zinc-300">
        <p>
          <span className="font-mono">{disk.volume}</span> is unplugged{isOn(vm) ? " from the running machine. Unmount it in the guest first to avoid losing writes" : ""}.
        </p>
        <Checkbox checked={del} onChange={setDel}>Also delete the volume file. This can't be undone.</Checkbox>
      </div>
    </Modal>
  );
}
