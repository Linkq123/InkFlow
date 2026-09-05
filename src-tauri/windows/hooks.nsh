!include "LogicLib.nsh"
!include "WinMessages.nsh"

!define INKFLOW_PATH_REGISTRY "Software\InkFlow"

LangString InkFlowAddToPath ${LANG_ENGLISH} "Add the InkFlow installation directory to the current user's PATH?$\r$\n$\r$\nNew terminals will then be able to run inkflow-cli directly."
LangString InkFlowAddToPath ${LANG_SIMPCHINESE} "是否将 InkFlow 安装目录加入当前用户 PATH？$\r$\n$\r$\n加入后可在新终端中直接运行 inkflow-cli。"
LangString InkFlowPathUpdateFailed ${LANG_ENGLISH} "InkFlow could not update the current user's PATH. No PATH value was changed."
LangString InkFlowPathUpdateFailed ${LANG_SIMPCHINESE} "InkFlow 无法更新当前用户 PATH，原 PATH 未被修改。"
LangString InkFlowPathRemovalAmbiguous ${LANG_ENGLISH} "InkFlow found multiple identical installation-directory entries in the current user's PATH. To avoid deleting an entry owned by you or another tool, PATH was left unchanged."
LangString InkFlowPathRemovalAmbiguous ${LANG_SIMPCHINESE} "InkFlow 在当前用户 PATH 中发现多个相同的安装目录。为避免删除由你或其他工具添加的项目，PATH 未作修改。"

!macro InkFlowBroadcastEnvironment
  System::Call 'USER32::SendMessageTimeout(i ${HWND_BROADCAST}, i ${WM_SETTINGCHANGE}, i 0, t "Environment", i 0x0002, i 5000, *i .r0)'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ${IfNot} ${Silent}
    MessageBox MB_YESNO|MB_DEFBUTTON2 "$(InkFlowAddToPath)" IDNO inkflow_path_done

    ; Never read PATH into an NSIS variable: the Unicode NSIS build used by
    ; Tauri has a 1024-character string limit and would silently truncate it.
    ; PowerShell reads and writes the registry value dynamically while
    ; preserving REG_SZ/REG_EXPAND_SZ and the original text.
    System::Call 'KERNEL32::SetEnvironmentVariable(t "INKFLOW_PATH_ENTRY", t "$INSTDIR") i .r0'
    nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$entry=[Environment]::GetEnvironmentVariable($\'INKFLOW_PATH_ENTRY$\',$\'Process$\'); $$key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($\'Environment$\',$$true); try { $$exists=$$key.GetValueNames() -contains $\'Path$\'; $$path=[string]$$key.GetValue($\'Path$\',$\'$\',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames); if ($$path.Split($\';$\') -contains $$entry) { exit 2 }; if ([string]::IsNullOrEmpty($$path)) { $$next=$$entry } elseif ($$path.EndsWith($\';$\')) { $$next=$$path+$$entry+$\';$\' } else { $$next=$$path+$\';$\'+$$entry }; if ($$exists) { $$kind=$$key.GetValueKind($\'Path$\') } else { $$kind=[Microsoft.Win32.RegistryValueKind]::ExpandString }; $$key.SetValue($\'Path$\',$$next,$$kind); [Console]::Out.Write([int]$$exists) } finally { $$key.Dispose() }"'
    Pop $0
    Pop $1
    System::Call 'KERNEL32::SetEnvironmentVariable(t "INKFLOW_PATH_ENTRY", i 0)'
    ${If} $0 == 0
      WriteRegStr HKCU "${INKFLOW_PATH_REGISTRY}" "PathEntry" "$INSTDIR"
      ${If} $1 == "0"
        WriteRegStr HKCU "${INKFLOW_PATH_REGISTRY}" "PathValueExisted" "0"
      ${Else}
        WriteRegStr HKCU "${INKFLOW_PATH_REGISTRY}" "PathValueExisted" "1"
      ${EndIf}
      !insertmacro InkFlowBroadcastEnvironment
    ${ElseIf} $0 != 2
      MessageBox MB_OK|MB_ICONEXCLAMATION "$(InkFlowPathUpdateFailed)"
    ${EndIf}
  ${EndIf}
  inkflow_path_done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ReadRegStr $1 HKCU "${INKFLOW_PATH_REGISTRY}" "PathEntry"
  ${If} $1 == "$INSTDIR"
    ReadRegStr $2 HKCU "${INKFLOW_PATH_REGISTRY}" "PathValueExisted"
    System::Call 'KERNEL32::SetEnvironmentVariable(t "INKFLOW_PATH_ENTRY", t "$1") i .r0'
    System::Call 'KERNEL32::SetEnvironmentVariable(t "INKFLOW_PATH_EXISTED", t "$2") i .r0'
    ; Remove the entry only when it is unique. Equal PATH values have no stable
    ; identity, so duplicates make ownership ambiguous and must be preserved.
    nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$entry=[Environment]::GetEnvironmentVariable($\'INKFLOW_PATH_ENTRY$\',$\'Process$\'); $$originalExisted=[Environment]::GetEnvironmentVariable($\'INKFLOW_PATH_EXISTED$\',$\'Process$\') -ne $\'0$\'; $$key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($\'Environment$\',$$true); try { $$path=[string]$$key.GetValue($\'Path$\',$\'$\',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames); $$all=@($$path.Split($\';$\')); $$indexes=@(); for ($$i=0; $$i -lt $$all.Count; $$i++) { if ([string]::Equals($$all[$$i],$$entry,[StringComparison]::OrdinalIgnoreCase)) { $$indexes += $$i } }; if ($$indexes.Count -eq 0) { exit 2 }; if ($$indexes.Count -ne 1) { exit 3 }; $$match=$$indexes[0]; $$kept=@(for ($$i=0; $$i -lt $$all.Count; $$i++) { if ($$i -ne $$match) { $$all[$$i] } }); if (-not $$originalExisted -and $$kept.Count -eq 0) { $$key.DeleteValue($\'Path$\',$$false) } else { $$kind=$$key.GetValueKind($\'Path$\'); $$key.SetValue($\'Path$\',[string]::Join($\';$\',$$kept),$$kind) } } finally { $$key.Dispose() }"'
    Pop $0
    Pop $2
    System::Call 'KERNEL32::SetEnvironmentVariable(t "INKFLOW_PATH_ENTRY", i 0)'
    System::Call 'KERNEL32::SetEnvironmentVariable(t "INKFLOW_PATH_EXISTED", i 0)'
    ${If} $0 == 0
      DeleteRegValue HKCU "${INKFLOW_PATH_REGISTRY}" "PathEntry"
      DeleteRegValue HKCU "${INKFLOW_PATH_REGISTRY}" "PathValueExisted"
      DeleteRegKey /ifempty HKCU "${INKFLOW_PATH_REGISTRY}"
      !insertmacro InkFlowBroadcastEnvironment
    ${ElseIf} $0 == 2
      ; The user already removed the exact entry, so only clear our marker.
      DeleteRegValue HKCU "${INKFLOW_PATH_REGISTRY}" "PathEntry"
      DeleteRegValue HKCU "${INKFLOW_PATH_REGISTRY}" "PathValueExisted"
      DeleteRegKey /ifempty HKCU "${INKFLOW_PATH_REGISTRY}"
    ${ElseIf} $0 == 3
      ; Duplicate values are indistinguishable. Relinquish ownership and leave
      ; all entries untouched instead of risking deletion of a user-owned one.
      DeleteRegValue HKCU "${INKFLOW_PATH_REGISTRY}" "PathEntry"
      DeleteRegValue HKCU "${INKFLOW_PATH_REGISTRY}" "PathValueExisted"
      DeleteRegKey /ifempty HKCU "${INKFLOW_PATH_REGISTRY}"
      ${IfNot} ${Silent}
        MessageBox MB_OK|MB_ICONINFORMATION "$(InkFlowPathRemovalAmbiguous)"
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend
