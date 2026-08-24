DetectHiddenWindows true
Persistent
#WinActivateForce
; #774: explicit Force single-instance. A second launch cleanly REPLACES the first (the ticket's
; named "chvíľu 2 procesy" double-start footgun) instead of leaving a stale instance behind, AND it
; re-runs the startup block below, which resets SafeLoop back to 1 -- self-healing a latched-off
; respawn guard (Alt+Q / the "No" MsgBox branch can otherwise stop respawns permanently until a
; manual restart). See scripts/strih/README.md for the deploy + capture-fidelity note.
#SingleInstance Force

TrayTip "Shortcuts", "Safe loop ZAPNUTY.", "Iconi"

app1_run := 1		; 1=zapne  0=nezapne
app1_path := "C:\ProgramData\Microsoft\Windows\Start Menu\Programs\OBS Studio.lnk"	;copy path v exploreri
app1_name := "ahk_exe obs64.exe"			;text ktory ma aplikacia v liste musi byt unikatny
app1_delay := 0 	;miliseconds
app1_runas := 0		;RunAs diferrent user
app1_user := 		;meno
app1_password :=	;heslo
app1_copybinary := 0

app2_run := 0
app2_path := "D:\_APPS\2ME-obs\2ME.lnk"
app2_name := "ahk_exe 2ME.exe"
app2_delay := 0


app3_run := 1
app3_path := "D:\_APPS\Resolume Arena.lnk"
app3_name := "ahk_exe Arena.exe"
app3_delay := 0

app4_run := 0
app4_path := "D:\companion-pc\sd adb run.ahk"
app4_name := "sd adb run"
app4_delay := 0
app4_reset := 0	;restartne ked je production

app5_run := 0
app5_path := "D:\_APPS\pab.lnk"
app5_name := "pab"
app5_delay := 0
app5_reset := 1	;restartne ked je production

app6_run := 1
app6_path := "D:\_APPS\tally.lnk"
app6_name := "tally"
app6_delay := 0

app7_run := 0
app7_path := "D:\_APPS\delay_control.lnk"
app7_name := "delay"
app7_delay := 0

SafeLoop := 1

app1()
{
	TrayTip "Automaticke zapinanie", "STLAC alt+Q ak chces vypnut." , "Icon!"
	Sleep 5000
	TrayTip "Automaticke zapinanie", "Zapínam " app1_name , "Iconi"
	Sleep app1_delay
	if(app1_runas)
		RunAs app1_user, app1_password
	; #1195: clear stale OBS crash sentinels before launching OBS, so a crash + this AHK
	; respawn never lands OBS on the "Run in Safe Mode?" modal (same cleanup the
	; launch-obs-genlock.sh wrapper does). Best-effort: zero matches or a locked file are fine.
	try {
		FileDelete A_AppData "\obs-studio\.sentinel\*"
	} catch {
		; no sentinels to clear, or a locked file -- launch anyway
	}
	Run app1_path
	Sleep 3000
	RunAs
}
app2()
{
	TrayTip "Automaticke zapinanie", "Zapínam " app2_name , "Iconi"
	Sleep app2_delay
	Run app2_path
	Sleep 3000
}
app3()
{
	TrayTip "Automaticke zapinanie", "Zapínam " app3_name , "Iconi"
	Sleep app3_delay
	Run app3_path
	Sleep 3000
}
app4()
{
	TrayTip "Automaticke zapinanie", "Zapínam " app4_name , "Iconi"
	Sleep app4_delay
	Run app4_path
	Sleep 3000
}
app5()
{
	TrayTip "Automaticke zapinanie", "Zapínam " app5_name , "Iconi"
	Sleep app5_delay
	Run app5_path
	Sleep 3000
}
app6()
{
	TrayTip "Automaticke zapinanie", "Zapínam " app6_name , "Iconi"
	Sleep app6_delay
	Run app6_path
	Sleep 3000
}
app7()
{
	TrayTip "Automaticke zapinanie", "Zapínam " app7_name , "Iconi"
	Sleep app7_delay
	Run app7_path
	Sleep 3000
}

;HLAVNE ZAPINANIE
Startup()
{
	While(true)
	{
		While(SafeLoop)
		{
			if (app1_run) and not WinExist(app1_name)
				app1()
			if (app2_run) and not WinExist(app2_name)
				app2()
			if (app3_run) and not WinExist(app3_name)
				app3()
			if (app4_run) and not WinExist(app4_name)
				app4()
			if (app5_run) and not WinExist(app5_name)
				app5()
			if (app6_run) and not WinExist(app6_name)
				app6()
			if (app7_run) and not WinExist(app7_name)
				app7()
			Sleep 1000
		}
		Sleep 5000
	}
}

if not WinExist(app1_name)
{
	result := MsgBox("Chces aby sa vsetko zaplo?",, "YesNo T10")
	if (result = "Yes")
	{
		TrayTip "Automaticke zapinanie", "Zapínam čakaj.", "Icon!"
		Startup()
	}
	if (result = "Timeout")
	{
		TrayTip "Automaticke zapinanie", "Zapínam čakaj.", "Icon!"
		Startup()
	}
	if (result = "No"){
		TrayTip "Automaticke zapinanie", "Nic sa nezapne.", "Icon!"
		global SafeLoop := 0
		Startup()
	}
}
else
	Startup()



!q::
{
	if (SafeLoop = 1)
	{
		TrayTip "Shortcuts", "Safe loop VYPNUTY.", "Iconi"
		global SafeLoop := 0
	}

	else
		TrayTip "Shortcuts", "AUTOMATICA NEBEZI!?.", "Icon!"

	;if WinExist(app1_name)
	;	WinKill app1_name
	;if WinExist(app2_name)
	;	WinKill app2_name
	;if WinExist(app3_name)
	;	WinKill app3_name
	;if WinExist(app5_name)
	;	WinKill app5_name
	;if WinExist(app6_name)
	;	WinKill app6_name



}

!l::
{
	if (SafeLoop = 0)
	{
		TrayTip "Shortcuts", "Safe loop ZAPNUTY.", "Iconi"
		global SafeLoop := 1
	}
	else
		TrayTip "Shortcuts", "Safe loop UZ BOL zapnuty.", "Iconi"
}

!p::
{
	TrayTip "Shortcuts", "PRODUCTION!", "Icon!"

	if WinExist(app1_name)
		WinActivate app1_name

	if WinExist(app2_name)
		WinActivate app2_name

	if WinExist(app3_name)
		WinActivate app3_name

	if WinExist(app4_name){
		WinActivate app4_name
		if (app4_reset)
			WinClose app4_name
	}

	if WinExist(app5_name){
		WinActivate app5_name
		if (app5_reset)
			WinClose app5_name
	}

	if WinExist(app6_name)
		WinActivate app6_name

	if WinExist(app7_name)
		WinActivate app7_name

	Send "!l"
}

!m::
{
	TrayTip "Shortcuts", "Zapinam modlitby streda.", "Iconi"
}
!n::
{
        if WinExist("vestibul")
		WinKill "vestibul"
}
!v::
{
	if not WinExist("vestibul")
		Run "D:\vestibul-pc\vestibul_obs\vestibul.lnk"
}


Startup()
