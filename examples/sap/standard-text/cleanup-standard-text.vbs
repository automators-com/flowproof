Option Explicit

Dim shell, name, sapGui, app, connection, session, status
Set shell = CreateObject("WScript.Shell")
name = shell.ExpandEnvironmentStrings("%SO10_TEXT_NAME%")
If name = "%SO10_TEXT_NAME%" Or Left(name, 4) <> "ZFP-" Then
  WScript.Echo "cleanup refused unsafe SO10_TEXT_NAME: " & name
  WScript.Quit 2
End If

Set sapGui = GetObject("SAPGUI")
Set app = sapGui.GetScriptingEngine
Set connection = app.Children(0)
Set session = connection.Children(0)

If session.Children.Count > 1 Then
  On Error Resume Next
  If session.FindById("wnd[1]").Text = "Delete Text" Then
    session.FindById("wnd[1]/usr/btnSPOP-OPTION1").Press
    WScript.Sleep 400
  Else
    session.FindById("wnd[1]").SendVKey 12
    WScript.Sleep 200
  End If
  On Error GoTo 0
End If

session.FindById("wnd[0]/tbar[0]/okcd").Text = "/nSO10"
session.FindById("wnd[0]").SendVKey 0
WScript.Sleep 400
session.FindById("wnd[0]/usr/ctxtRSSCE-TDNAME").Text = name
session.FindById("wnd[0]/usr/btn%#AUTOTEXT003").Press
WScript.Sleep 400
status = session.FindById("wnd[0]/sbar").Text
If Not IsAbsent(status) Then
  If InStr(1, session.FindById("wnd[0]").Text, name, vbTextCompare) = 0 Then
    WScript.Echo "cleanup could not locate tagged text " & name & ": " & status
    WScript.Quit 3
  End If
  session.FindById("wnd[0]/mbar/menu[0]/menu[9]").Select
  WScript.Sleep 300
  session.FindById("wnd[1]/usr/btnSPOP-OPTION1").Press
  WScript.Sleep 500
End If

session.FindById("wnd[0]/tbar[0]/okcd").Text = "/nSO10"
session.FindById("wnd[0]").SendVKey 0
WScript.Sleep 300
session.FindById("wnd[0]/usr/ctxtRSSCE-TDNAME").Text = name
session.FindById("wnd[0]/usr/btn%#AUTOTEXT002").Press
WScript.Sleep 400
status = session.FindById("wnd[0]/sbar").Text
If Not IsAbsent(status) Then
  WScript.Echo "cleanup verification failed for " & name & ": " & status
  WScript.Quit 4
End If
WScript.Echo "removed SO10 sandbox text " & name

Function IsAbsent(message)
  IsAbsent = InStr(1, message, "not found", vbTextCompare) > 0 Or _
    InStr(1, message, "does not exist", vbTextCompare) > 0
End Function
