#!/usr/bin/env python3
"""obs-box-power (issue 1464): the power button at the left end of the Linux OBS boxes' kiosk panel.

WHY (owner, 23.10.2026: "ikonkna ka lavo dole v liste na vypnutie a restert pc ze si otvorim a potvrdim
co chcem"): the openbox kiosk had no shutdown UI at all; the operator could only hold the power key.

WHAT (design issuecomment-6363142461, Approach 1): the panel's first launcher, `Vypnúť`, runs this file.
It opens ONE small Gtk 3 dialog in Slovak with three buttons, `Zrušiť` / `Reštartovať` / `Vypnúť`:

  * `Zrušiť` is the default response and holds the focus, so Enter / Space on an unread dialog cancels;
    Escape and closing the window cancel too. A cancel runs nothing.
  * Only the confirmed `Vypnúť` / `Reštartovať` runs exactly ONE command: `systemctl poweroff` /
    `systemctl reboot` (logind). The OBS unit then stops through the normal shutdown, which saves the
    collection.
  * A refused command (logind / polkit said no, an inhibitor) shows a message dialog naming systemctl's
    own error, never a silent nothing; the program exits 1.

The dialog keeps above every other window (a fullscreen Multiview projector would otherwise cover it).
The kiosk's tint2 runs as the --user unit obs-box-panel.service, so this program and its systemctl run
in the user manager, outside any logind session; polkit attributes such a process to the user's display
session (the active local seat0 autologin), which logind's default policy lets power off / reboot without
a password. verify-strih item 44's (panel-power) row reads CanPowerOff / CanReboot in that SAME context
(`systemd-run --user`), so the grade and the button agree by construction.

The toolkit module is an argument of PowerDialog, so tests/python/test_obs_box_power_1464.py drives the
real wiring with a stand-in Gtk (no display and no gi in CI).
"""
import subprocess
import sys

TITLE = "Vypnúť počítač"
QUESTION = "Čo chceš s týmto počítačom urobiť?"
DETAIL = "Vypnutie aj reštart najprv riadne ukončia OBS a ostatné aplikácie."
LABEL_CANCEL = "Zrušiť"
LABEL_REBOOT = "Reštartovať"
LABEL_POWEROFF = "Vypnúť"
# Gtk.ResponseType values are all negative; the two confirmations use our own positive ids.
RESPONSE_POWEROFF = 1
RESPONSE_REBOOT = 2
COMMANDS = {
    RESPONSE_POWEROFF: ["systemctl", "poweroff"],
    RESPONSE_REBOOT: ["systemctl", "reboot"],
}
COMMAND_TIMEOUT_S = 30


def _log(msg):
    print(msg, file=sys.stderr, flush=True)


def command_for(response):
    """The command a dialog response confirms, or None (cancel, Escape, close, anything unknown)."""
    cmd = COMMANDS.get(response)
    return list(cmd) if cmd else None


def _run_command(cmd):
    """Runs cmd bounded; returns (rc, error text). A missing binary or a timeout is a failure too."""
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=COMMAND_TIMEOUT_S, check=False)
    except (OSError, subprocess.TimeoutExpired) as e:
        return 1, str(e)
    return r.returncode, (r.stderr or r.stdout or "").strip()


class PowerDialog:
    """The confirm dialog. Gtk is gi.repository.Gtk in production; tests pass a stand-in with the same
    calls. run_cmd(cmd) -> (rc, error text) is the command runner (a seam for the tests)."""

    def __init__(self, Gtk, run_cmd=_run_command, log=_log):
        self._gtk = Gtk
        self._run_cmd = run_cmd
        self._log = log
        dlg = Gtk.Dialog(title=TITLE)
        dlg.set_keep_above(True)
        dlg.set_position(Gtk.WindowPosition.CENTER)
        dlg.set_resizable(False)
        cancel = dlg.add_button(LABEL_CANCEL, Gtk.ResponseType.CANCEL)
        dlg.add_button(LABEL_REBOOT, RESPONSE_REBOOT)
        dlg.add_button(LABEL_POWEROFF, RESPONSE_POWEROFF)
        dlg.set_default_response(Gtk.ResponseType.CANCEL)
        box = dlg.get_content_area()
        box.set_spacing(8)
        box.add(Gtk.Label(label=QUESTION))
        box.add(Gtk.Label(label=DETAIL))
        dlg.show_all()
        # after show_all: the focus lands on Zrušiť, so a stray Enter / Space never confirms
        cancel.grab_focus()
        self.dialog = dlg

    def run(self):
        """Shows the dialog, runs the confirmed command if any. Returns the process exit code."""
        response = self.dialog.run()
        self.dialog.destroy()
        cmd = command_for(response)
        if cmd is None:
            self._log("obs-box-power: cancelled (response %r) -- nothing done" % (response,))
            return 0
        self._log("obs-box-power: confirmed -- running %s" % " ".join(cmd))
        rc, err = self._run_cmd(cmd)
        if rc == 0:
            return 0
        self._log("obs-box-power: %s FAILED (rc %s): %s" % (" ".join(cmd), rc, err or "no output"))
        self._show_error(cmd, err)
        return 1

    def _show_error(self, cmd, err):
        Gtk = self._gtk
        msg = Gtk.MessageDialog(message_type=Gtk.MessageType.ERROR, buttons=Gtk.ButtonsType.OK,
                                text="Príkaz „%s“ sa nepodaril." % " ".join(cmd))
        msg.format_secondary_text(err or "systemctl nevypísal dôvod.")
        msg.set_keep_above(True)
        msg.run()
        msg.destroy()


def main(argv=None):
    del argv
    import gi
    gi.require_version("Gtk", "3.0")
    from gi.repository import Gtk

    ok, _rest = Gtk.init_check(sys.argv)
    if not ok:
        _log("obs-box-power: no X display -- nothing shown, nothing done")
        return 1
    return PowerDialog(Gtk).run()


if __name__ == "__main__":
    sys.exit(main())
