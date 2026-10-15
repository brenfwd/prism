/** Builds cloud-init `#cloud-config` user-data from friendly fields. */

export interface CloudInitForm {
  hostname: string;
  username: string;
  sshKeys: string;
  password: string;
  packages: string[];
  upgrade: boolean;
}

/** JSON strings are valid YAML double-quoted scalars. */
const q = (s: string) => JSON.stringify(s);

export function sshKeyLines(text: string): string[] {
  return text
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith("#"));
}

const KEY_RE = /^(ssh-(rsa|ed25519|dss)|ecdsa-sha2-nistp\d+|sk-(ssh-ed25519|ecdsa-sha2-nistp256)@openssh\.com)\s+[A-Za-z0-9+/=]+(\s.*)?$/;

/** The first line that isn't an OpenSSH public key, if any. */
export function invalidKey(text: string): string | null {
  for (const l of sshKeyLines(text)) if (!KEY_RE.test(l)) return l;
  return null;
}

export function userData(f: CloudInitForm): string {
  const keys = sshKeyLines(f.sshKeys);
  const out = ["#cloud-config", `hostname: ${q(f.hostname)}`, "manage_etc_hosts: true", "users:"];
  out.push(
    `  - name: ${q(f.username)}`,
    "    shell: /bin/bash",
    "    sudo: \"ALL=(ALL) NOPASSWD:ALL\"",
    `    lock_passwd: ${f.password ? "false" : "true"}`,
  );
  if (f.password) out.push(`    plain_text_passwd: ${q(f.password)}`);
  if (keys.length) {
    out.push("    ssh_authorized_keys:");
    for (const k of keys) out.push(`      - ${q(k)}`);
  }
  out.push(`ssh_pwauth: ${f.password ? "true" : "false"}`);
  if (f.password) out.push("chpasswd:", "  expire: false");
  const pkgs = f.packages.filter(Boolean);
  if (pkgs.length || f.upgrade) out.push("package_update: true");
  if (f.upgrade) out.push("package_upgrade: true");
  if (pkgs.length) {
    out.push("packages:");
    for (const p of pkgs) out.push(`  - ${q(p)}`);
  }
  if (pkgs.includes("qemu-guest-agent")) {
    out.push("runcmd:", "  - [systemctl, enable, --now, qemu-guest-agent]");
  }
  return out.join("\n") + "\n";
}

/** A sensible default login for well-known cloud images. */
export function defaultUser(image: string): string {
  const n = image.toLowerCase();
  for (const d of ["ubuntu", "debian", "fedora", "centos", "rocky", "alma", "arch", "opensuse"]) {
    if (n.includes(d)) return d === "alma" ? "almalinux" : d === "rocky" ? "rocky" : d;
  }
  return "admin";
}
