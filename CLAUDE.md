# Claude Code Guidelines for camera-box

## Script Failure Policy

**IMPORTANT:** When a setup script or automation script fails:

1. **DO NOT** manually run commands to complete the failed steps
2. **DO** fix the script to handle the failure case
3. **DO** re-run the fixed script from the beginning
4. **DO** commit the script fix before proceeding

This ensures:
- Scripts are always complete and self-contained
- Future runs will succeed without manual intervention
- No undocumented manual steps exist in the deployment process

## Device Setup

When setting up a new camera device:
- Use `scripts/setup.sh` - it handles everything
- NDI library must be copied manually (licensing restriction)
- Device registry is in `SETUP.md`

## Configuration Reference

| Setting | Correct Value | Example Result |
|---------|---------------|----------------|
| `ndi_name` | `"usb"` | CAM2 (usb) |
| `hostname` | Device name | CAM2 |
| `intercom.stream` | Lowercase device | cam2 |

## IP Assignment

| Device | IP Address |
|--------|------------|
| CAM1 | 10.77.9.61 |
| CAM2 | 10.77.9.62 |
| CAM3 | 10.77.9.63 |
| CAM4 | 10.77.9.64 |

## Build & Deploy

**IMPORTANT:** Use IP addresses, not hostnames (`.lan` DNS may not resolve):

```bash
# Build release
cargo build --release

# Deploy to device (use IP from table above, password: newlevel)
sshpass -p 'newlevel' ssh root@10.77.9.6X "mount -o remount,rw / && systemctl stop camera-box"
sshpass -p 'newlevel' scp target/release/camera-box root@10.77.9.6X:/usr/local/bin/
sshpass -p 'newlevel' ssh root@10.77.9.6X "systemctl start camera-box && mount -o remount,ro / 2>/dev/null; true"
```

Note: `rw-mode`/`ro-mode` scripts may not exist on all devices. Use `mount -o remount,rw /` instead.
