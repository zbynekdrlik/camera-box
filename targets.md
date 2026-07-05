# Deployment Targets

## Windows Targets (DanteSync)

| Host | IP Address | Status | Notes |
|------|------------|--------|-------|
| stagebox1 | 10.77.9.237 | Active | SSH: newlevel/newlevel |
| strih | 10.77.9.202 | Active | SSH: newlevel/newlevel |
| ableton-foh | 10.77.9.230 | Active | SSH: master/master |
| mbc | 10.77.9.232 | Active | SSH: newlevel/newlevel |
| stream | 10.77.9.204 | Active | SSH: newlevel/newlevel |
| bridge | 10.77.9.201 | Active | SSH: newlevel/newlevel |
| iem | 10.77.9.231 | Active | SSH: iem/iem |
| songs | 10.77.9.212 | Active | SSH: newlevel/newlevel |
| piano | 10.77.9.236 | Offline | SSH: newlevel/newlevel |

## Camera Targets (camera-box)

| Device | IP Address | Status | Notes |
|--------|------------|--------|-------|
| CAM1 | 10.77.9.61 | Active | READ-ONLY reference |
| CAM2 | 10.77.9.62 | Active | SSH: root/newlevel |
| CAM3 | 10.77.9.63 | Active | SSH: root/newlevel |
| CAM4 | 10.77.9.64 | Active | SSH: root/newlevel |
| CAM5 | 10.77.9.65 | Provisioning | SSH: root/newlevel; fleet growing 4->7 (#451) |
| CAM6 | 10.77.9.66 | Provisioning | SSH: root/newlevel; fleet growing 4->7 (#451) |
| CAM7 | 10.77.9.67 | Provisioning | SSH: root/newlevel; fleet growing 4->7 (#451) |

## Linux OBS Targets (camera-box, #458)

| Host | IP Address | Status | Notes |
|------|------------|--------|-------|
| imag-nb | 10.77.9.182 | Active | SSH: newlevel/newlevel (sudo needs pw); 60fps IMAG OBS box, genlock hot-swap over PPA base (#460); dev1 also has headless key-based SSH (`~/.ssh/id_ed25519`) for `scripts/drift-guard.sh --check-imag` (#541), installed by `setup-imag.sh` step 19 |

## Important Notes

- **Always use IP addresses**, not `.lan` hostnames (DNS may not resolve)
- Camera devices use `mount -o remount,rw /` (not `rw-mode` command)
- Windows targets use `newlevel` user (not `root`)
- imag-nb (Linux OBS) also uses `newlevel` user (live-verified SSH login), same as most Windows targets
