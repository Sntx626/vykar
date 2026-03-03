---
name: vm-image
description: "Validate dedupe efficiency by mutating and backing up a mounted Ubuntu VM disk image twice"
---

# VM Image Dedupe (guestmount + chroot, or boot+ssh via KVM)

## Goal

Validate dedupe efficiency on large binary VM disk images by:
1. Taking a first backup after baseline OS mutations
2. Applying additional changes
3. Taking a second backup
4. Measuring repository growth between backups

This scenario intentionally stresses chunk-level dedupe in a realistic "daily VM drift" workflow.

## Scope

- **Backend**: `local` (required baseline)
- **Image**: Ubuntu Noble cloud image converted to raw
- **Mutation method**:
  - `guestmount` + `chroot` (offline)
  - boot+ssh with QEMU/KVM (online, preferred for apt realism)
- **Primary metric**: repo byte growth from snapshot 1 to snapshot 2

## Prerequisites

1. Install image tooling:
   ```bash
   sudo apt-get update
   sudo apt-get install -y libguestfs-tools qemu-utils cloud-image-utils openssh-client socat
   ```
2. Freeze/thaw support requires QEMU guest agent in guest + guest agent socket on host:
   - ensure `qemu-guest-agent` is installed and running in the VM
   - start QEMU with `-chardev socket,path=/tmp/qga.sock,server=on,wait=off,id=qga0`
3. Ensure KVM access for non-sudo QEMU:
   ```bash
   sudo usermod -aG kvm "$USER"
   # Apply group in current shell (or re-login):
   newgrp kvm
   # Alternative one-shot wrapper:
   # sg kvm -c 'qemu-system-x86_64 ...'
   ```
4. Export passphrase:
   ```bash
   export VGER_PASSPHRASE=123
   ```
5. Use a dedicated work area:
   ```bash
   mkdir -p ~/runtime/vm-image/{repo,src,mnt,logs,reports}
   ```
6. Create scenario config from `~/vger.sample.yaml` with:
   - local repo path under `~/runtime/vm-image/repo`
   - source path `~/runtime/vm-image/src`
   - source label `vm-image`
   - source hooks for guest freeze/thaw (preferred over inline shell wrapping)

For libvirt-hosted VM images (your specific case), use source hooks like this:

```yaml
sources:
  - path: /var/lib/libvirt/images
    label: vm-images
    hooks:
      before: >
        echo '{"execute":"guest-fsfreeze-freeze"}' |
        socat - unix-connect:/tmp/qga.sock
      finally: >
        echo '{"execute":"guest-fsfreeze-thaw"}' |
        socat - unix-connect:/tmp/qga.sock
```

For this vm-image scenario (`~/runtime/vm-image/src`), use the same hook pattern on that source label/path.

## Image Preparation

1. Download Ubuntu cloud image:
   ```bash
   cd ~/runtime/vm-image
   wget -O noble-server-cloudimg-amd64.img https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img
   ```
2. Convert qcow2 to raw:
   ```bash
   qemu-img convert -f qcow2 -O raw noble-server-cloudimg-amd64.img vm-disk.raw
   ```
3. Place raw image under source directory:
   ```bash
   cp vm-disk.raw src/vm-disk.raw
   ```

## Online Mutation Path (KVM + SSH, RECOMMENDED)

Use this path when you want real in-guest package behavior (snapd/systemd interactions) instead of chroot-only behavior.

### 1. Build cloud-init seed (SSH + password)

```bash
cat > ~/runtime/vm-image/user-data <<EOF
#cloud-config
password: test1234
chpasswd: { expire: false }
ssh_pwauth: true
EOF
cloud-localds ~/runtime/vm-image/seed.img ~/runtime/vm-image/user-data
```

### 2. Start VM with KVM + port forward

```bash
qemu-system-x86_64 \
  -m 3072 \
  -smp 2 \
  -enable-kvm \
  -drive file=~/runtime/vm-image/src/vm-disk.raw,format=raw \
  -drive file=~/runtime/vm-image/seed.img,format=raw \
  -display none \
  -netdev user,id=net0,hostfwd=tcp::2222-:22 \
  -device virtio-net-pci,netdev=net0 \
  -chardev socket,path=/tmp/qga.sock,server=on,wait=off,id=qga0 \
  -device virtio-serial \
  -device virtserialport,chardev=qga0,name=org.qemu.guest_agent.0 \
  -daemonize \
  -pidfile ~/runtime/vm-image/qemu.pid
```

Wait for SSH:

```bash
for _ in $(seq 1 180); do
  ssh -i ~/.ssh/id_ed25519 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=6 -p 2222 ubuntu@127.0.0.1 'echo up' && break
  sleep 2
done
```

### 3. DNS rule for QEMU user-net guests

Inside this environment, public resolvers (e.g. `1.1.1.1`) may fail from guest.
Use QEMU slirp resolver `10.0.2.3`:

```bash
ssh -i ~/.ssh/id_ed25519 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -p 2222 ubuntu@127.0.0.1 \
  "echo -e 'nameserver 10.0.2.3\noptions timeout:2 attempts:2' | sudo tee /etc/resolv.conf"
```

### 4. Connection and apt hardening (recommended)

Use timeout-bounded SSH and apt flags to avoid indefinite hangs:

```bash
SSH_BASE="ssh -i ~/.ssh/id_ed25519 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=6 -p 2222 ubuntu@127.0.0.1"
guest_run() { timeout 600 bash -lc "$SSH_BASE \"$*\""; }
APT_OPTS="-o Acquire::ForceIPv4=true -o Acquire::Retries=3 -o Acquire::http::Timeout=20 -o Acquire::https::Timeout=20 -o Dpkg::Lock::Timeout=120"

# Remove noisy sudo host-resolution warning (optional, but recommended)
guest_run "grep -q '^127.0.1.1 ubuntu$' /etc/hosts || echo '127.0.1.1 ubuntu' | sudo tee -a /etc/hosts"
```

### 5. Phase 1 (booted guest) mutations + backup #1 (freeze/thaw via hooks)

```bash
# In-guest changes
guest_run "sudo DEBIAN_FRONTEND=noninteractive apt-get $APT_OPTS update"
guest_run "sudo DEBIAN_FRONTEND=noninteractive apt-get $APT_OPTS install -y ubuntu-desktop-minimal"
guest_run "sudo bash -lc 'mkdir -p /var/log/vger-test; seq 1 200 | xargs -I{} dd if=/dev/urandom of=/var/log/vger-test/log-{}.bin bs=64K count=1 status=none'"

# Backup #1
# Freeze/thaw is handled by source hooks.
vger --config ~/runtime/vm-image/config.vm-image.yaml delete -R local --yes-delete-this-repo || true
vger --config ~/runtime/vm-image/config.vm-image.yaml init -R local
vger --config ~/runtime/vm-image/config.vm-image.yaml backup -R local -l vm-image ~/runtime/vm-image/src
vger --config ~/runtime/vm-image/config.vm-image.yaml list -R local --last 3 | tee ~/runtime/vm-image/logs/vm-image-list-after-backup1.log
du -sb ~/runtime/vm-image/repo | tee ~/runtime/vm-image/logs/vm-image-repo-size-after-backup1.txt
```

### 6. Phase 2 (booted guest) mutations + backup #2 (freeze/thaw via hooks)

Keep the same running VM, then:

```bash
# Keep DNS pinned to slirp resolver
guest_run "echo -e 'nameserver 10.0.2.3\noptions timeout:2 attempts:2' | sudo tee /etc/resolv.conf"

# Between-backup mutations (record these in logs)
guest_run "sudo DEBIAN_FRONTEND=noninteractive apt-get $APT_OPTS update"
guest_run "sudo DEBIAN_FRONTEND=noninteractive apt-get $APT_OPTS install -y --no-install-recommends thunderbird libreoffice-core htop curl jq git"
guest_run "sudo bash -lc 'mkdir -p /opt/vger-day2; seq 1 100 | xargs -I{} dd if=/dev/urandom of=/opt/vger-day2/day2-{}.bin bs=128K count=1 status=none'"

# Backup #2
# Freeze/thaw is handled by source hooks.
vger --config ~/runtime/vm-image/config.vm-image.yaml backup -R local -l vm-image ~/runtime/vm-image/src
vger --config ~/runtime/vm-image/config.vm-image.yaml list -R local --last 5 | tee ~/runtime/vm-image/logs/vm-image-list-after-backup2.log
du -sb ~/runtime/vm-image/repo | tee ~/runtime/vm-image/logs/vm-image-repo-size-after-backup2.txt
vger --config ~/runtime/vm-image/config.vm-image.yaml check -R local
```

## Mount/Unmount Helpers (RECOMMENDED)

Use these helpers to keep mount lifecycle deterministic:

```bash
mount_vm() {
  local img="$1"
  local mnt="$2"
  sudo mkdir -p "$mnt"
  sudo guestmount -a "$img" -i "$mnt"
  sudo mount --bind /dev "$mnt/dev"
  sudo mount --bind /proc "$mnt/proc"
  sudo mount --bind /sys "$mnt/sys"
  sudo mount --bind /run "$mnt/run"
  sudo cp /etc/resolv.conf "$mnt/etc/resolv.conf"
}

umount_vm() {
  local mnt="$1"
  sudo umount -lf "$mnt/run" 2>/dev/null || true
  sudo umount -lf "$mnt/sys" 2>/dev/null || true
  sudo umount -lf "$mnt/proc" 2>/dev/null || true
  sudo umount -lf "$mnt/dev" 2>/dev/null || true
  sudo guestunmount "$mnt"
}
```

## Phase 1 — Baseline Mutations + Backup #1

1. Mount image:
   ```bash
   mount_vm ~/runtime/vm-image/src/vm-disk.raw ~/runtime/vm-image/mnt
   ```
2. Run baseline package operations inside chroot:
   ```bash
   sudo chroot ~/runtime/vm-image/mnt apt-get update
   sudo chroot ~/runtime/vm-image/mnt apt-get install -y --no-install-recommends ubuntu-desktop-minimal
   ```
3. Add deterministic churn files:
   ```bash
   sudo chroot ~/runtime/vm-image/mnt bash -lc 'mkdir -p /var/log/vger-test && for i in $(seq 1 200); do dd if=/dev/urandom of=/var/log/vger-test/log-$i.bin bs=64K count=1 status=none; done'
   ```
4. Unmount image:
   ```bash
   umount_vm ~/runtime/vm-image/mnt
   ```
5. Reset/init local repo:
   ```bash
   vger --config ~/runtime/vm-image/config.vm-image.yaml delete -R local --yes-delete-this-repo || true
   vger --config ~/runtime/vm-image/config.vm-image.yaml init -R local
   ```
6. Run first backup:
   ```bash
   vger --config ~/runtime/vm-image/config.vm-image.yaml backup -R local -l vm-image ~/runtime/vm-image/src
   ```
7. Record snapshot ID and repo size:
   ```bash
   vger --config ~/runtime/vm-image/config.vm-image.yaml list -R local --last 3 | tee ~/runtime/vm-image/logs/vm-image-list-after-backup1.log
   du -sb ~/runtime/vm-image/repo | tee ~/runtime/vm-image/logs/vm-image-repo-size-after-backup1.txt
   sha256sum ~/runtime/vm-image/src/vm-disk.raw | tee ~/runtime/vm-image/logs/vm-image-sha-after-backup1.txt
   ```

## Phase 2 — More Mutations + Backup #2

1. Re-mount image:
   ```bash
   mount_vm ~/runtime/vm-image/src/vm-disk.raw ~/runtime/vm-image/mnt
   ```
2. Apply incremental changes:
   ```bash
   sudo chroot ~/runtime/vm-image/mnt apt-get update
   sudo chroot ~/runtime/vm-image/mnt bash -lc 'apt-get install -y --no-install-recommends htop curl jq'
   sudo chroot ~/runtime/vm-image/mnt bash -lc 'mkdir -p /opt/vger-daily && for i in $(seq 1 100); do dd if=/dev/urandom of=/opt/vger-daily/day2-$i.bin bs=128K count=1 status=none; done'
   ```
3. Unmount image:
   ```bash
   umount_vm ~/runtime/vm-image/mnt
   ```
4. Run second backup:
   ```bash
   vger --config ~/runtime/vm-image/config.vm-image.yaml backup -R local -l vm-image ~/runtime/vm-image/src
   ```
5. Record second snapshot and repo growth:
   ```bash
   vger --config ~/runtime/vm-image/config.vm-image.yaml list -R local --last 5 | tee ~/runtime/vm-image/logs/vm-image-list-after-backup2.log
   du -sb ~/runtime/vm-image/repo | tee ~/runtime/vm-image/logs/vm-image-repo-size-after-backup2.txt
   sha256sum ~/runtime/vm-image/src/vm-disk.raw | tee ~/runtime/vm-image/logs/vm-image-sha-after-backup2.txt
   ```

## Validation

1. Both backups exit 0 and produce distinct snapshot IDs.
2. `vger --config <config> check -R local` exits 0.
3. Snapshot 2 repository size increase is materially smaller than full raw image size.
4. Repo growth ratio is documented:
   - `growth_bytes = size_after_backup2 - size_after_backup1`
   - `growth_ratio = growth_bytes / raw_image_bytes`
5. Restore latest snapshot and verify image hash matches source hash:
   ```bash
   RESTORE_DIR=~/runtime/vm-image/restore-latest
   rm -rf "$RESTORE_DIR" && mkdir -p "$RESTORE_DIR"
   SNAPSHOT_ID="$(vger --config ~/runtime/vm-image/config.vm-image.yaml list -R local --last 1 | awk 'NR==2 {print $1}')"
   vger --config ~/runtime/vm-image/config.vm-image.yaml restore -R local "$SNAPSHOT_ID" "$RESTORE_DIR"
   RESTORED_RAW="$(find "$RESTORE_DIR" -maxdepth 3 -type f -name 'vm-disk.raw' | head -n1)"
   sha256sum ~/runtime/vm-image/src/vm-disk.raw "$RESTORED_RAW"
   ```

## Failure Cases to Record

- `guestmount` cannot auto-detect partition (`-i`) or mount fails
- chroot apt operations fail due to missing bind mounts or DNS
- KVM launch fails (`/dev/kvm` permission denied, missing `kvm` group in current shell)
- SSH port 2222 accepts TCP but hangs before banner (guest boot instability)
- cloud-init credentials not reapplied on reused disk image
- guest agent socket missing/unresponsive (`/tmp/qga.sock`, `guest-fsfreeze-*` failures)
- source hooks misconfigured or not attached to the active source label/path
- apt hangs due to network/lock contention; use timeout-bounded SSH + `APT_OPTS` in this guide
- install exceeds available space inside cloud image
- `guestunmount` fails due to lingering bind mounts
- second backup stores near-full image size increase (unexpected dedupe regression)

## Common Issues

- Cloud images are sparse; always use `du -sb` on repo and keep `qemu-img info` output in logs
- For booted guest with QEMU user networking, use `nameserver 10.0.2.3` inside guest; public resolvers may fail
- Package installs may trigger service starts; these are acceptable as long as chroot exits 0
- If desktop package is too heavy for image free space, use a smaller package set and document deviation
- Always unmount bind mounts before `guestunmount` to avoid busy mount errors

## Cleanup

1. Ensure image is unmounted:
   ```bash
   umount_vm ~/runtime/vm-image/mnt || true
   ```
2. Optional repo teardown:
   ```bash
   vger --config ~/runtime/vm-image/config.vm-image.yaml delete -R local --yes-delete-this-repo || true
   ```
3. Keep logs and report files under `~/runtime/vm-image/logs` and `~/runtime/vm-image/reports`.
