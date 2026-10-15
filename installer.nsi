; Per-user installer. Every removal is an exact owned file, never a directory tree.
Unicode true
!define MUI_ICON "${APP_ICON}"
!define MUI_UNICON "${APP_ICON}"
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "WordFunc.nsh"
!include "WinVer.nsh"
!include "x64.nsh"
!include "${PAYLOAD_DEFINES}"
!ifndef APP_NAME
  !define APP_NAME "Compositor"
!endif
!ifndef APP_ID
  !define APP_ID "Compositor.Windows.7D47274A"
!endif
!define OWN_KEY "Software\${APP_ID}"
!define ARP_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_ID}"
Var WelcomeText
Name "${APP_NAME}"
OutFile "${SETUP_OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\${APP_NAME}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
SetCompressorDictSize 16
SetOverwrite off
CRCCheck on
ManifestDPIAware true
; The portable payload is an x64 build and uses the Windows 10 API baseline.
; Windows 11 uses the same supportedOS compatibility identifier.
ManifestSupportedOS Win10
Icon "${APP_ICON}"
UninstallIcon "${APP_ICON}"
VIProductVersion "${WINDOWS_VERSION}"
VIAddVersionKey /LANG=2052 "ProductName" "${APP_NAME}"
VIAddVersionKey /LANG=2052 "FileDescription" "${APP_NAME} 安装程序"
VIAddVersionKey /LANG=2052 "FileVersion" "${WINDOWS_VERSION}"
VIAddVersionKey /LANG=2052 "ProductVersion" "${APP_VERSION}"
VIAddVersionKey /LANG=2052 "LegalCopyright" "Compositor contributors; see App 开源软件许可证"
!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TITLE "安装 ${APP_NAME} ${APP_VERSION}"
!define MUI_WELCOMEPAGE_TEXT "$WelcomeText"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"

Var ExpectedDirectory
Var Handles
Var HandleCount
Var Staging
Var OwnedInstall
Var FilePath
Var ExpectedHash
Var ObservedHash
Var ExpectedIdentity
Var ObservedIdentity
Var DeleteMode
Var FileResult
Var FileOpenError
Var FileRetry
Var Preserved
Var InstalledVersion
Var HeldHandle
Var OldFiles
Var NewFiles
Var RenameHandle
Var RenameResult
Var CommitCount
Var BackupCount
Var TransactionFailed

; Directory handles omit FILE_SHARE_DELETE. The final path and reparse attributes
; are checked after opening, so a junction swap cannot redirect later file work.
!macro DirectoryFunctions PREFIX
Function ${PREFIX}LockChain
  Exch $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  lock_loop:
    StrCmp $0 "" lock_done
    StrLen $1 $0
    StrCmp $1 2 0 +4
    StrCpy $1 $0 1 1
    StrCmp $1 ":" 0 +2
      StrCpy $0 "$0\"
    System::Call 'kernel32::GetFileAttributesW(w r0)i.r1'
    IntCmp $1 -1 lock_parent
    IntOp $2 $1 & 0x410
    IntCmp $2 0x10 0 lock_bad lock_bad
    System::Call 'kernel32::CreateFileW(w r0, i 0, i 3, p 0, i 3, i 0x02200000, p 0)p.r2 ?!e'
    IntCmp $2 -1 lock_bad
    System::Alloc 52
    Pop $5
    System::Call 'kernel32::GetFileInformationByHandle(p r2, p r5)i.r4'
    System::Call '*$5(i.r3)'
    System::Free $5
    IntCmp $4 0 lock_close_bad
    IntOp $3 $3 & 0x410
    IntCmp $3 0x10 0 lock_close_bad lock_close_bad
    System::Call 'kernel32::GetFinalPathNameByHandleW(p r2, w .r3, i ${NSIS_MAX_STRLEN}, i 0)i.r4'
    IntCmp $4 0 lock_close_bad
    IntCmp $4 ${NSIS_MAX_STRLEN} lock_close_bad 0 lock_close_bad
    StrCpy $3 $3 "" 4
    StrCmp $3 $0 0 lock_close_bad
    System::Call '*$Handles(p r2)'
    IntOp $Handles $Handles + ${NSIS_PTR_SIZE}
    IntOp $HandleCount $HandleCount + 1
    IntCmp $HandleCount 250 lock_bad 0 lock_bad
  lock_parent:
    StrLen $1 $0
    StrCmp $1 3 0 +3
    StrCpy $1 $0 1 1
    StrCmp $1 ":" lock_done
    ${GetParent} "$0" $1
    StrCmp $0 $1 lock_done
    StrCpy $0 $1
    Goto lock_loop
  lock_close_bad:
    System::Call 'kernel32::CloseHandle(p r2)'
  lock_bad:
    DetailPrint "路径检查失败：$0"
    SetErrorLevel 20
    MessageBox MB_ICONSTOP|MB_OK "安装/卸载已停止：目录有重解析点、路径发生变化或正在使用。$\r$\n$0" /SD IDOK
    Quit
  lock_done:
    Pop $5
    Pop $4
    Pop $3
    Pop $2
    Pop $1
    Pop $0
FunctionEnd

Function ${PREFIX}ReleaseLocks
  Push $0
  release_loop:
    IntCmp $HandleCount 0 release_done
    IntOp $Handles $Handles - ${NSIS_PTR_SIZE}
    System::Call '*$Handles(p.r0)'
    System::Call 'kernel32::CloseHandle(p r0)'
    IntOp $HandleCount $HandleCount - 1
    Goto release_loop
  release_done:
    System::Free $Handles
    Pop $0
FunctionEnd

Function ${PREFIX}InitializePaths
  SetShellVarContext current
  SetRegView 64
  StrCpy $ExpectedDirectory "$LOCALAPPDATA\Programs\${APP_NAME}"
  System::Call 'kernel32::GetFullPathNameW(w "$INSTDIR", i ${NSIS_MAX_STRLEN}, w .r0, p 0)i.r1'
  IntCmp $1 0 unsafe_destination
  IntCmp $1 ${NSIS_MAX_STRLEN} unsafe_destination 0 unsafe_destination
  StrCmp $0 $ExpectedDirectory 0 unsafe_destination
  StrCpy $INSTDIR $ExpectedDirectory
  System::Alloc 2048
  Pop $Handles
  StrCpy $HandleCount 0
  StrCpy $Preserved 0
  Push $INSTDIR
  Call ${PREFIX}LockChain
  Return
  unsafe_destination:
    SetErrorLevel 21
    MessageBox MB_ICONSTOP|MB_OK "为保护其他目录，本安装器只接受当前用户的固定安装目录：$\r$\n$ExpectedDirectory" /SD IDOK
    Quit
FunctionEnd
!macroend
!insertmacro DirectoryFunctions ""
!insertmacro DirectoryFunctions "un."

; Hash and optional deletion use one native handle. Sharing excludes writes and
; deletion; FileDispositionInfo removes precisely the opened name, including
; only that hardlink. No recursive deletion, wildcard or reboot deletion exists.
!macro FileFunctions PREFIX
Function ${PREFIX}ProcessOwnedFile
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  Push $6
  Push $7
  Push $8
  Push $9
  StrCpy $ObservedHash ""
  StrCpy $ObservedIdentity ""
  StrCpy $FileResult 0
  StrCpy $FileOpenError 0
  StrCpy $HeldHandle 0
  StrCpy $1 0
  StrCpy $2 0
  StrCpy $3 0
  StrCpy $4 0
  StrCpy $5 0
  StrCpy $6 0
  StrCpy $0 $FilePath
  StrCpy $7 0x80000000
  StrCmp $DeleteMode 0 +2
    IntOp $7 $7 | 0x00010000
  ; Write access is only reserved, never used: SEC_IMAGE mappings reject it,
  ; and holding it prevents an application launch while its files are replaced.
  StrCmp $DeleteMode 2 reserve_write
  StrCmp $DeleteMode 3 0 open_file
  reserve_write:
    IntOp $7 $7 | 0x40000000
  open_file:
  System::Call 'kernel32::CreateFileW(w r0, i r7, i 1, p 0, i 3, i 0x00200000, p 0)p.r1 ?e'
  Pop $FileOpenError
  IntCmp $1 -1 file_finish
  StrCpy $FileOpenError 0
  System::Alloc 52
  Pop $2
  System::Call 'kernel32::GetFileInformationByHandle(p r1, p r2)i.r7'
  IntCmp $7 0 file_finish
  System::Call '*$2(i.r7)'
  IntOp $7 $7 & 0x410
  IntCmp $7 0 0 file_finish file_finish
  IntOp $7 $2 + 40
  System::Call '*$7(i.r8)'
  IntCmp $8 1 0 file_finish file_finish
  IntOp $7 $2 + 28
  System::Call '*$7(i.r8)'
  IntFmt $8 "%08X" $8
  StrCpy $ObservedIdentity $8
  IntOp $7 $2 + 44
  System::Call '*$7(i.r8, i.r9)'
  IntFmt $8 "%08X" $8
  IntFmt $9 "%08X" $9
  StrCpy $ObservedIdentity "$ObservedIdentity-$8$9"
  System::Call 'advapi32::CryptAcquireContextW(*p.r3, p 0, p 0, i 24, i 0xF0000000)i.r7'
  IntCmp $7 0 file_finish
  System::Call 'advapi32::CryptCreateHash(p r3, i 0x800c, p 0, i 0, *p.r4)i.r7'
  IntCmp $7 0 file_finish
  System::Alloc 65536
  Pop $5
  hash_read:
    System::Call 'kernel32::ReadFile(p r1, p r5, i 65536, *i.r8, p 0)i.r7'
    IntCmp $7 0 file_finish
    IntCmp $8 0 hash_finish
    System::Call 'advapi32::CryptHashData(p r4, p r5, i r8, i 0)i.r7'
    IntCmp $7 0 file_finish
    Goto hash_read
  hash_finish:
    System::Alloc 32
    Pop $6
    StrCpy $8 32
    System::Call 'advapi32::CryptGetHashParam(p r4, i 2, p r6, *i r8, i 0)i.r7'
    IntCmp $7 0 file_finish
    StrCpy $8 0
    StrCpy $ObservedHash ""
  hash_hex:
    IntOp $7 $6 + $8
    System::Call '*$7(&i1.r9)'
    IntFmt $9 "%02X" $9
    StrCpy $ObservedHash "$ObservedHash$9"
    IntOp $8 $8 + 1
    IntCmp $8 32 hash_checked hash_hex hash_hex
  hash_checked:
    StrCpy $FileResult 1
    StrCmp $DeleteMode 0 file_finish
    StrCmp $ObservedHash $ExpectedHash 0 file_preserved
    StrCmp $DeleteMode 3 file_hold
    StrCmp $ExpectedIdentity "" file_preserved
    StrCmp $ObservedIdentity $ExpectedIdentity 0 file_preserved
    StrCmp $DeleteMode 2 file_hold
    System::Call '*$2(i 1)'
    System::Call 'kernel32::SetFileInformationByHandle(p r1, i 4, p r2, i 4)i.r7'
    IntCmp $7 0 file_preserved
    DetailPrint "移除本软件文件：$FilePath"
    Goto file_finish
  file_hold:
    StrCpy $HeldHandle $1
    StrCpy $1 -1
    Goto file_finish
  file_preserved:
    StrCpy $FileResult 0
  file_finish:
    StrCmp $6 0 +2
      System::Free $6
    StrCmp $5 0 +2
      System::Free $5
    StrCmp $4 0 +2
      System::Call 'advapi32::CryptDestroyHash(p r4)'
    StrCmp $3 0 +2
      System::Call 'advapi32::CryptReleaseContext(p r3, i 0)'
    StrCmp $2 0 +2
      System::Free $2
    IntCmp $1 -1 +2
      System::Call 'kernel32::CloseHandle(p r1)'
    Pop $9
    Pop $8
    Pop $7
    Pop $6
    Pop $5
    Pop $4
    Pop $3
    Pop $2
    Pop $1
    Pop $0
FunctionEnd
!macroend
!insertmacro FileFunctions ""
!insertmacro FileFunctions "un."

; Compare only the stable numeric versions emitted by our package script.
Function ValidateInstalledVersion
  Push $0
  Push $1
  Push $2
  Push $3
  StrCpy $0 0
  StrCpy $1 0
  StrCpy $2 ""
  version_character:
    StrCpy $3 $InstalledVersion 1 $0
    StrCmp $3 "" version_end
    StrCmp $3 "." version_component
    StrCmp $3 "0" version_digit
    StrCmp $3 "1" version_digit
    StrCmp $3 "2" version_digit
    StrCmp $3 "3" version_digit
    StrCmp $3 "4" version_digit
    StrCmp $3 "5" version_digit
    StrCmp $3 "6" version_digit
    StrCmp $3 "7" version_digit
    StrCmp $3 "8" version_digit
    StrCmp $3 "9" version_digit version_invalid
  version_digit:
    StrCpy $2 "$2$3"
    StrLen $3 $2
    IntCmp $3 5 0 0 version_invalid
    IntOp $0 $0 + 1
    Goto version_character
  version_component:
    StrCmp $2 "" version_invalid
    IntCmp $2 65535 0 0 version_invalid
    StrCpy $2 ""
    IntOp $1 $1 + 1
    IntCmp $1 2 0 0 version_invalid
    IntOp $0 $0 + 1
    Goto version_character
  version_end:
    StrCmp $2 "" version_invalid
    IntCmp $2 65535 0 0 version_invalid
    StrCmp $1 2 0 version_invalid
    Pop $3
    Pop $2
    Pop $1
    Pop $0
    Return
  version_invalid:
    SetErrorLevel 28
    MessageBox MB_ICONSTOP|MB_OK "安装已停止：现有版本记录缺失或不一致，已保留原程序和文件。" /SD IDOK
    Quit
FunctionEnd

; Renaming through the verified file handle preserves the old FileID and keeps
; its backup locked until commit or rollback has finished.
Function RenameVerifiedHandle
  Push $0
  Push $1
  Push $2
  Push $3
  StrLen $0 $FilePath
  IntOp $1 $0 * 2
  IntOp $2 $1 + 12
  IntOp $3 ${NSIS_MAX_STRLEN} * 2
  IntOp $3 $3 + 12
  System::Alloc $3
  Pop $0
  System::Call '*$0(i 0, p 0, i r1, &w${NSIS_MAX_STRLEN} "$FilePath")'
  System::Call 'kernel32::SetFileInformationByHandle(p $RenameHandle, i 3, p r0, i r2)i.s'
  Pop $RenameResult
  System::Free $0
  Pop $3
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

!macro TransactionFile INDEX NAME HASH
  !define /redef TRANSACTION_ID ${__COUNTER__}
  IntOp $0 ${INDEX} * ${NSIS_PTR_SIZE}
  IntOp $0 $OldFiles + $0
  System::Call '*$0(p 0)'
  IfFileExists "$INSTDIR\${NAME}" 0 prepare_new_${TRANSACTION_ID}
    ReadRegStr $ExpectedHash HKCU "${OWN_KEY}" "SHA256:${NAME}"
    ReadRegStr $ExpectedIdentity HKCU "${OWN_KEY}" "Identity:SHA256:${NAME}"
    StrCpy $FilePath "$INSTDIR\${NAME}"
    StrCpy $DeleteMode 2
    Call ProcessOwnedFile
    StrCmp $FileResult 1 0 transaction_busy
    System::Call '*$0(p $HeldHandle)'
  prepare_new_${TRANSACTION_ID}:
  StrCpy $ExpectedHash "${HASH}"
  StrCpy $FilePath "$Staging\${NAME}"
  StrCpy $DeleteMode 3
  Call ProcessOwnedFile
  StrCmp $FileResult 1 0 transaction_invalid
  IntOp $0 ${INDEX} * ${NSIS_PTR_SIZE}
  IntOp $0 $NewFiles + $0
  System::Call '*$0(p $HeldHandle)'
!macroend

!macro CommitFile INDEX NAME
  !define /redef COMMIT_ID ${__COUNTER__}
  IntOp $0 ${INDEX} * ${NSIS_PTR_SIZE}
  IntOp $0 $OldFiles + $0
  System::Call '*$0(p.s)'
  Pop $RenameHandle
  StrCmp $RenameHandle 0 commit_new_${COMMIT_ID}
    StrCpy $FilePath "$Staging\backup-${NAME}"
    Call RenameVerifiedHandle
    StrCmp $RenameResult 1 0 transaction_rollback
  commit_new_${COMMIT_ID}:
  IntOp $BackupCount ${INDEX} + 1
  IntOp $0 ${INDEX} * ${NSIS_PTR_SIZE}
  IntOp $0 $NewFiles + $0
  System::Call '*$0(p.s)'
  Pop $RenameHandle
  StrCpy $FilePath "$INSTDIR\${NAME}"
!ifdef QA_FAIL_COMMIT_AT
!if ${INDEX} == ${QA_FAIL_COMMIT_AT}
  Goto transaction_rollback
!endif
!endif
  Call RenameVerifiedHandle
  StrCmp $RenameResult 1 0 transaction_rollback
  IntOp $CommitCount ${INDEX} + 1
!macroend

!macro RestoreFile INDEX NAME
  !define /redef RESTORE_ID ${__COUNTER__}
  IntCmp $CommitCount ${INDEX} restore_old_${RESTORE_ID} restore_old_${RESTORE_ID} 0
    IntOp $0 ${INDEX} * ${NSIS_PTR_SIZE}
    IntOp $0 $NewFiles + $0
    System::Call '*$0(p.s)'
    Pop $RenameHandle
    StrCpy $FilePath "$Staging\${NAME}"
    Call RenameVerifiedHandle
    StrCmp $RenameResult 1 +2
      StrCpy $TransactionFailed 1
  restore_old_${RESTORE_ID}:
  IntCmp $BackupCount ${INDEX} restore_done_${RESTORE_ID} restore_done_${RESTORE_ID} 0
    IntOp $0 ${INDEX} * ${NSIS_PTR_SIZE}
    IntOp $0 $OldFiles + $0
    System::Call '*$0(p.s)'
    Pop $RenameHandle
    StrCmp $RenameHandle 0 restore_done_${RESTORE_ID}
    StrCpy $FilePath "$INSTDIR\${NAME}"
    Call RenameVerifiedHandle
    StrCmp $RenameResult 1 +2
      StrCpy $TransactionFailed 1
  restore_done_${RESTORE_ID}:
!macroend

!macro PublishFile INDEX NAME HASH
  IntOp $0 ${INDEX} * ${NSIS_PTR_SIZE}
  IntOp $0 $NewFiles + $0
  System::Call '*$0(p.r1)'
  System::Alloc 52
  Pop $2
  System::Call 'kernel32::GetFileInformationByHandle(p r1, p r2)i.r3'
  IntOp $3 $2 + 28
  System::Call '*$3(i.r4)'
  IntFmt $4 "%08X" $4
  IntOp $3 $2 + 44
  System::Call '*$3(i.r5, i.r6)'
  IntFmt $5 "%08X" $5
  IntFmt $6 "%08X" $6
  System::Free $2
  WriteRegStr HKCU "${OWN_KEY}" "SHA256:${NAME}" "${HASH}"
  WriteRegStr HKCU "${OWN_KEY}" "Identity:SHA256:${NAME}" "$4-$5$6"
!macroend

Function CloseTransactionHandles
  Push $0
  Push $1
  Push $2
  Push $3
  StrCpy $0 0
  close_file_handle:
    IntOp $1 $0 * ${NSIS_PTR_SIZE}
    IntOp $2 $OldFiles + $1
    System::Call '*$2(p.r3)'
    StrCmp $3 0 +2
      System::Call 'kernel32::CloseHandle(p r3)'
    IntOp $2 $NewFiles + $1
    System::Call '*$2(p.r3)'
    StrCmp $3 0 +2
      System::Call 'kernel32::CloseHandle(p r3)'
    IntOp $0 $0 + 1
    IntCmp $0 8 close_transaction_done close_file_handle close_transaction_done
  close_transaction_done:
    System::Free $OldFiles
    System::Free $NewFiles
    Pop $3
    Pop $2
    Pop $1
    Pop $0
FunctionEnd

Function DeleteCommittedBackups
  Push $0
  Push $1
  Push $2
  Push $3
  System::Alloc 4
  Pop $3
  System::Call '*$3(i 1)'
  StrCpy $0 0
  delete_verified_backup:
    IntOp $1 $0 * ${NSIS_PTR_SIZE}
    IntOp $1 $OldFiles + $1
    System::Call '*$1(p.r2)'
    StrCmp $2 0 +2
      System::Call 'kernel32::SetFileInformationByHandle(p r2, i 4, p r3, i 4)i.s'
    StrCmp $2 0 +2
      Pop $1
    IntOp $0 $0 + 1
    IntCmp $0 8 delete_backups_done delete_verified_backup delete_backups_done
  delete_backups_done:
    System::Free $3
    Pop $3
    Pop $2
    Pop $1
    Pop $0
FunctionEnd

Function DiscardStagedFiles
  Push $0
  Push $1
  Push $2
  Push $3
  System::Alloc 4
  Pop $3
  System::Call '*$3(i 1)'
  StrCpy $0 0
  discard_staged_file:
    IntOp $1 $0 * ${NSIS_PTR_SIZE}
    IntOp $1 $NewFiles + $1
    System::Call '*$1(p.r2)'
    StrCmp $2 0 +2
      System::Call 'kernel32::SetFileInformationByHandle(p r2, i 4, p r3, i 4)i.s'
    StrCmp $2 0 +2
      Pop $1
    IntOp $0 $0 + 1
    IntCmp $0 8 discard_staged_done discard_staged_file discard_staged_done
  discard_staged_done:
    System::Free $3
    Pop $3
    Pop $2
    Pop $1
    Pop $0
FunctionEnd

!macro CheckExisting NAME
  !define /redef CHECK_ID ${__COUNTER__}
  IfFileExists "$INSTDIR\${NAME}" 0 check_done_${CHECK_ID}
  StrCmp $OwnedInstall 1 0 install_collision
  ReadRegStr $ExpectedHash HKCU "${OWN_KEY}" "SHA256:${NAME}"
  ReadRegStr $ExpectedIdentity HKCU "${OWN_KEY}" "Identity:SHA256:${NAME}"
  StrCmp $ExpectedHash "" install_collision
  StrCmp $ExpectedIdentity "" install_collision
  StrCpy $FilePath "$INSTDIR\${NAME}"
  StrCpy $DeleteMode 0
  Call ProcessOwnedFile
  StrCmp $FileResult 1 0 install_collision
  StrCmp $ObservedHash $ExpectedHash 0 install_collision
  StrCmp $ObservedIdentity $ExpectedIdentity 0 install_collision
  check_done_${CHECK_ID}:
!macroend

Function .onInit
  ; Fail before touching the destination on unsupported systems.  The check is
  ; deliberately x64 + Windows 10 or newer: the payload cannot run on 32-bit
  ; Windows, and Windows 7/8 do not provide the supported graphics/runtime
  ; baseline used by the application.
  ${IfNot} ${RunningX64}
    SetErrorLevel 30
    MessageBox MB_ICONSTOP|MB_OK "此安装包仅支持 64 位 Windows 10 或 Windows 11。当前系统不是 64 位 Windows，安装已停止。" /SD IDOK
    Quit
  ${EndIf}
  ${IfNot} ${AtLeastWin10}
    SetErrorLevel 31
    MessageBox MB_ICONSTOP|MB_OK "此版本仅支持 Windows 10（22H2）或 Windows 11。当前系统版本过低，安装已停止。" /SD IDOK
    Quit
  ${EndIf}
  ; The application targets the Windows 10 22H2 API/driver baseline.
  ; Windows 11 build numbers are higher than 19045, so this single
  ; numeric gate covers both supported families without relying on a product
  ; name that can be virtualized by compatibility settings.
  SetRegView 64
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "CurrentBuildNumber"
  StrCmp $0 "" 0 +2
    ReadRegStr $0 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "CurrentBuild"
  IntCmp $0 19045 win10_build_ok win10_build_too_low win10_build_ok
  win10_build_too_low:
    SetErrorLevel 32
    MessageBox MB_ICONSTOP|MB_OK "此版本仅支持 Windows 10（22H2）或 Windows 11。当前系统版本过低，安装已停止。" /SD IDOK
    Quit
  win10_build_ok:
  ; NSIS removes its special /D switch from $CMDLINE; inspect the OS command line.
  System::Call 'kernel32::GetCommandLineW()w.r0'
  ClearErrors
  ${GetOptions} $0 "/D=" $1
  IfErrors +2
    StrCpy $INSTDIR $1
  Call InitializePaths
  ReadRegStr $0 HKCU "${OWN_KEY}" "AppId"
  ReadRegStr $1 HKCU "${OWN_KEY}" "InstallLocation"
  StrCpy $OwnedInstall 0
  StrCmp $0 "${APP_ID}" own_location
  StrCmp $0 "${APP_ID}:2" 0 initial_directory
  own_location:
  StrCmp $1 $INSTDIR 0 initial_directory
  StrCpy $OwnedInstall 1
  initial_directory:
  StrCmp $OwnedInstall 1 owned_registration
  IfFileExists "$INSTDIR\*.*" install_collision
  ReadRegStr $0 HKCU "${ARP_KEY}" "DisplayName"
  StrCmp $0 "" existing_files install_collision
  owned_registration:
  ReadRegStr $0 HKCU "${ARP_KEY}" "InstallLocation"
  StrCmp $0 $INSTDIR 0 install_collision
  ReadRegStr $0 HKCU "${ARP_KEY}" "UninstallString"
  StrCmp $0 '$\"$INSTDIR\uninstall.exe$\"' 0 install_collision
  ReadRegStr $0 HKCU "${ARP_KEY}" "QuietUninstallString"
  StrCmp $0 '$\"$INSTDIR\uninstall.exe$\" /S' 0 install_collision
  ReadRegStr $0 HKCU "${ARP_KEY}" "DisplayIcon"
  StrCmp $0 "$INSTDIR\compositor.exe,0" 0 install_collision
  ReadRegStr $InstalledVersion HKCU "${OWN_KEY}" "Version"
  StrCmp $InstalledVersion "" 0 check_version_record
  ; The published 0.1.0 installer recorded only ARP DisplayVersion.
  ReadRegStr $InstalledVersion HKCU "${ARP_KEY}" "DisplayVersion"
  check_version_record:
  ReadRegStr $0 HKCU "${ARP_KEY}" "DisplayVersion"
  StrCmp $0 $InstalledVersion 0 version_record_changed
  Call ValidateInstalledVersion
  ${VersionCompare} "$InstalledVersion" "${APP_VERSION}" $0
  StrCmp $0 1 version_downgrade
  existing_files:
  !insertmacro CheckExisting "compositor.exe"
  !insertmacro CheckExisting "README.md"
  !insertmacro CheckExisting "LICENSES.md"
  !insertmacro CheckExisting "SKILL.md"
  !insertmacro CheckExisting "USER_AGENT_SKILL.md"
  !insertmacro CheckExisting "runtime-imports.json"
  !insertmacro CheckExisting "package-files.json"
  !insertmacro CheckExisting "uninstall.exe"
  StrCpy $WelcomeText "新版：${APP_VERSION}$\r$\n安装到当前用户的程序目录，无需管理员权限。$\r$\n$\r$\n卸载只移除本程序安装的文件；工程、设置及自行添加的文件会保留。"
  StrCmp $OwnedInstall 1 0 +2
    StrCpy $WelcomeText "当前版本：$InstalledVersion$\r$\n新版：${APP_VERSION}$\r$\n$\r$\n继续将覆盖升级本程序，请先关闭正在运行的 ${APP_NAME}。$\r$\n工程、设置和自行添加的文件会保留。"
  Return
  version_record_changed:
    SetErrorLevel 28
    MessageBox MB_ICONSTOP|MB_OK "安装已停止：现有版本记录不一致，已保留原程序和文件。" /SD IDOK
    Quit
  version_downgrade:
    SetErrorLevel 27
    MessageBox MB_ICONSTOP|MB_OK "已安装版本 $InstalledVersion 高于本安装包 ${APP_VERSION}，为保护现有工程和设置，已停止降级。" /SD IDOK
    Quit
  install_collision:
    SetErrorLevel 22
    MessageBox MB_ICONSTOP|MB_OK "安装已停止：目标中有未经确认的文件，或已有程序文件被修改。请保留文件并使用便携版。$\r$\n$INSTDIR" /SD IDOK
    Quit
FunctionEnd

!macro InstallShortcut BASE NAME VALUE
  !define /redef SHORTCUT_ID ${__COUNTER__}
  IfFileExists "${BASE}\${NAME}.lnk" 0 shortcut_create_${SHORTCUT_ID}
    ReadRegStr $ExpectedHash HKCU "${OWN_KEY}" "${VALUE}"
    ReadRegStr $ExpectedIdentity HKCU "${OWN_KEY}" "Identity:${VALUE}"
    StrCmp $ExpectedHash "" shortcut_done_${SHORTCUT_ID}
    StrCpy $FilePath "${BASE}\${NAME}.lnk"
    StrCpy $DeleteMode 1
    Call ProcessOwnedFile
    StrCmp $FileResult 1 0 shortcut_done_${SHORTCUT_ID}
  shortcut_create_${SHORTCUT_ID}:
    CreateShortCut "$Staging\${VALUE}.lnk" "$INSTDIR\compositor.exe" "" "$INSTDIR\compositor.exe" 0
    StrCpy $FilePath "$Staging\${VALUE}.lnk"
    StrCpy $DeleteMode 0
    Call ProcessOwnedFile
    StrCmp $FileResult 1 0 shortcut_done_${SHORTCUT_ID}
    ClearErrors
    Rename "$Staging\${VALUE}.lnk" "${BASE}\${NAME}.lnk"
    IfErrors shortcut_done_${SHORTCUT_ID}
    StrCpy $FilePath "${BASE}\${NAME}.lnk"
    StrCpy $DeleteMode 0
    Call ProcessOwnedFile
    StrCmp $FileResult 1 0 shortcut_done_${SHORTCUT_ID}
    WriteRegStr HKCU "${OWN_KEY}" "${VALUE}" "$ObservedHash"
    WriteRegStr HKCU "${OWN_KEY}" "Identity:${VALUE}" "$ObservedIdentity"
  shortcut_done_${SHORTCUT_ID}:
!macroend

!macro CleanupStagedFile NAME HASH
  !define /redef CLEANUP_ID ${__COUNTER__}
  StrCpy $FilePath "$Staging\${NAME}"
  IfFileExists "$FilePath" 0 cleanup_done_${CLEANUP_ID}
  StrCpy $DeleteMode 0
  Call ProcessOwnedFile
  StrCmp $FileResult 1 0 cleanup_done_${CLEANUP_ID}
  StrCmp $ObservedHash "${HASH}" 0 cleanup_done_${CLEANUP_ID}
  StrCpy $ExpectedHash $ObservedHash
  StrCpy $ExpectedIdentity $ObservedIdentity
  StrCpy $DeleteMode 1
  Call ProcessOwnedFile
  cleanup_done_${CLEANUP_ID}:
!macroend

Function CleanupStage
  !insertmacro CleanupStagedFile "compositor.exe" "${HASH_EXE}"
  !insertmacro CleanupStagedFile "README.md" "${HASH_README}"
  !insertmacro CleanupStagedFile "LICENSES.md" "${HASH_LICENSES}"
  !insertmacro CleanupStagedFile "SKILL.md" "${HASH_SKILL}"
  !insertmacro CleanupStagedFile "USER_AGENT_SKILL.md" "${HASH_USER_SKILL}"
  !insertmacro CleanupStagedFile "runtime-imports.json" "${HASH_RUNTIME}"
  !insertmacro CleanupStagedFile "package-files.json" "${HASH_MANIFEST}"
  !insertmacro CleanupStagedFile "uninstall.exe" "$R9"
FunctionEnd

Section "${APP_NAME}" SEC_APP
  ; Validate and pin shortcut paths before replacing any application file.
  Push "$DESKTOP"
  Call LockChain
  Push "$SMPROGRAMS"
  Call LockChain
  ; Lock existing ancestors before creating directories; then lock the new leaf.
  CreateDirectory "$INSTDIR"
  Push $INSTDIR
  Call LockChain
  ; A unique native-created staging directory is never recursively removed.
  System::Call 'ole32::CoCreateGuid(g .r0)i.r1'
  StrCmp $1 0 0 staging_error
  StrCpy $Staging "$INSTDIR\.install-$0"
  System::Call 'kernel32::CreateDirectoryW(w "$Staging", p 0)i.r1'
  StrCmp $1 0 staging_error
  Push $Staging
  Call LockChain
  SetOutPath "$Staging"
  File /oname=compositor.exe "${PAYLOAD_DIR}\compositor.exe"
  File /oname=README.md "${PAYLOAD_DIR}\README.md"
  File /oname=LICENSES.md "${PAYLOAD_DIR}\LICENSES.md"
  File /oname=SKILL.md "${PAYLOAD_DIR}\SKILL.md"
  File /oname=USER_AGENT_SKILL.md "${PAYLOAD_DIR}\USER_AGENT_SKILL.md"
  File /oname=runtime-imports.json "${PAYLOAD_DIR}\runtime-imports.json"
  File /oname=package-files.json "${PAYLOAD_DIR}\package-files.json"
  WriteUninstaller "$Staging\uninstall.exe"
  StrCpy $FilePath "$Staging\uninstall.exe"
  StrCpy $DeleteMode 0
  Call ProcessOwnedFile
  StrCmp $FileResult 1 0 staging_error
  StrCpy $R9 $ObservedHash
  System::Alloc 64
  Pop $OldFiles
  System::Alloc 64
  Pop $NewFiles
  StrCpy $0 0
  initialize_file_handles:
    IntOp $1 $0 * ${NSIS_PTR_SIZE}
    IntOp $2 $OldFiles + $1
    System::Call '*$2(p 0)'
    IntOp $2 $NewFiles + $1
    System::Call '*$2(p 0)'
    IntOp $0 $0 + 1
    IntCmp $0 8 prepare_transaction initialize_file_handles prepare_transaction
  prepare_transaction:
  StrCpy $CommitCount 0
  StrCpy $BackupCount 0
  StrCpy $TransactionFailed 0
  ; Acquire every original before moving any of them. A late occupied file or
  ; running application therefore rejects the upgrade with the old version intact.
  !insertmacro TransactionFile 0 "compositor.exe" "${HASH_EXE}"
  !insertmacro TransactionFile 1 "README.md" "${HASH_README}"
  !insertmacro TransactionFile 2 "LICENSES.md" "${HASH_LICENSES}"
  !insertmacro TransactionFile 3 "SKILL.md" "${HASH_SKILL}"
  !insertmacro TransactionFile 4 "runtime-imports.json" "${HASH_RUNTIME}"
  !insertmacro TransactionFile 5 "package-files.json" "${HASH_MANIFEST}"
  !insertmacro TransactionFile 6 "uninstall.exe" "$R9"
  !insertmacro TransactionFile 7 "USER_AGENT_SKILL.md" "${HASH_USER_SKILL}"
  !insertmacro CommitFile 0 "compositor.exe"
  !insertmacro CommitFile 1 "README.md"
  !insertmacro CommitFile 2 "LICENSES.md"
  !insertmacro CommitFile 3 "SKILL.md"
  !insertmacro CommitFile 4 "runtime-imports.json"
  !insertmacro CommitFile 5 "package-files.json"
  !insertmacro CommitFile 6 "uninstall.exe"
  !insertmacro CommitFile 7 "USER_AGENT_SKILL.md"
  ; Publish ownership only after the entire file set has committed.
  !insertmacro PublishFile 0 "compositor.exe" "${HASH_EXE}"
  !insertmacro PublishFile 1 "README.md" "${HASH_README}"
  !insertmacro PublishFile 2 "LICENSES.md" "${HASH_LICENSES}"
  !insertmacro PublishFile 3 "SKILL.md" "${HASH_SKILL}"
  !insertmacro PublishFile 4 "runtime-imports.json" "${HASH_RUNTIME}"
  !insertmacro PublishFile 5 "package-files.json" "${HASH_MANIFEST}"
  !insertmacro PublishFile 6 "uninstall.exe" "$R9"
  !insertmacro PublishFile 7 "USER_AGENT_SKILL.md" "${HASH_USER_SKILL}"
  WriteRegStr HKCU "${OWN_KEY}" "AppId" "${APP_ID}:2"
  WriteRegStr HKCU "${OWN_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${OWN_KEY}" "Version" "${APP_VERSION}"
  Call DeleteCommittedBackups
  !insertmacro InstallShortcut "$DESKTOP" "${APP_NAME}" "DesktopShortcut"
  !insertmacro InstallShortcut "$SMPROGRAMS" "${APP_NAME}" "StartMenuShortcut"
  WriteRegStr HKCU "${ARP_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "${ARP_KEY}" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "${ARP_KEY}" "Publisher" "Compositor contributors"
  WriteRegStr HKCU "${ARP_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${ARP_KEY}" "DisplayIcon" "$INSTDIR\compositor.exe,0"
  WriteRegStr HKCU "${ARP_KEY}" "UninstallString" '$\"$INSTDIR\uninstall.exe$\"'
  WriteRegStr HKCU "${ARP_KEY}" "QuietUninstallString" '$\"$INSTDIR\uninstall.exe$\" /S'
  WriteRegDWORD HKCU "${ARP_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${ARP_KEY}" "NoRepair" 1
  Call CloseTransactionHandles
  SetOutPath "$LOCALAPPDATA"
  Call ReleaseLocks
  RMDir "$Staging"
  SetErrorLevel 0
  Goto installation_done
  transaction_busy:
    SetErrorLevel 23
    StrCpy $WelcomeText "旧版程序或文件正在使用或已变化。请关闭 ${APP_NAME} 后重新运行安装包。旧版本已保留。"
    Goto transaction_abort
  transaction_invalid:
    SetErrorLevel 25
    StrCpy $WelcomeText "暂存文件验证失败，已保留原程序。请重新运行完整安装包。"
    Goto transaction_abort
  transaction_rollback:
    SetErrorLevel 24
    !insertmacro RestoreFile 7 "USER_AGENT_SKILL.md"
    !insertmacro RestoreFile 6 "uninstall.exe"
    !insertmacro RestoreFile 5 "package-files.json"
    !insertmacro RestoreFile 4 "runtime-imports.json"
    !insertmacro RestoreFile 3 "SKILL.md"
    !insertmacro RestoreFile 2 "LICENSES.md"
    !insertmacro RestoreFile 1 "README.md"
    !insertmacro RestoreFile 0 "compositor.exe"
    StrCpy $WelcomeText "文件替换失败，已恢复原版本。请关闭程序后重试。"
    StrCmp $TransactionFailed 0 transaction_abort
    StrCpy $WelcomeText "文件替换失败。原文件备份已保留在：$\r$\n$Staging$\r$\n请保留此目录。"
  transaction_abort:
    StrCmp $TransactionFailed 0 0 +2
      Call DiscardStagedFiles
    Call CloseTransactionHandles
    StrCmp $TransactionFailed 0 0 +2
      Call CleanupStage
    SetOutPath "$LOCALAPPDATA"
    Call ReleaseLocks
    RMDir "$Staging"
    Abort "$WelcomeText"
  staging_error:
    SetErrorLevel 25
    Abort "创建或验证暂存文件失败，已停止安装。"
  installation_done:
SectionEnd

Function un.onInit
  System::Call 'kernel32::GetCommandLineW()w.r0'
  ClearErrors
  ${GetOptions} $0 "_?=" $1
  IfErrors +2
    StrCpy $INSTDIR $1
  Call un.InitializePaths
  ReadRegStr $0 HKCU "${OWN_KEY}" "AppId"
  StrCmp $0 "${APP_ID}:2" 0 uninstall_wrong_owner
  ReadRegStr $0 HKCU "${OWN_KEY}" "Version"
  StrCmp $0 "${APP_VERSION}" 0 uninstall_wrong_owner
  ReadRegStr $0 HKCU "${OWN_KEY}" "InstallLocation"
  StrCmp $0 $INSTDIR 0 uninstall_wrong_owner
  ReadRegStr $0 HKCU "${ARP_KEY}" "InstallLocation"
  StrCmp $0 $INSTDIR 0 uninstall_wrong_owner
  ReadRegStr $0 HKCU "${ARP_KEY}" "UninstallString"
  StrCmp $0 '$\"$INSTDIR\uninstall.exe$\"' 0 uninstall_wrong_owner
  ReadRegStr $0 HKCU "${ARP_KEY}" "QuietUninstallString"
  StrCmp $0 '$\"$INSTDIR\uninstall.exe$\" /S' 0 uninstall_wrong_owner
  Return
  uninstall_wrong_owner:
    SetErrorLevel 26
    MessageBox MB_ICONSTOP|MB_OK "卸载已停止：未找到此目录对应的本软件安装记录。" /SD IDOK
    Quit
FunctionEnd

!macro UninstallFile NAME VALUE
  !define /redef UNINSTALL_ID ${__COUNTER__}
  StrCpy $FilePath "${NAME}"
  IfFileExists "$FilePath" 0 uninstall_done_${UNINSTALL_ID}
  ReadRegStr $ExpectedHash HKCU "${OWN_KEY}" "${VALUE}"
  ReadRegStr $ExpectedIdentity HKCU "${OWN_KEY}" "Identity:${VALUE}"
  StrCmp $ExpectedHash "" uninstall_preserve_${UNINSTALL_ID}
  StrCpy $DeleteMode 1
  StrCpy $FileRetry 0
  uninstall_retry_${UNINSTALL_ID}:
  Call un.ProcessOwnedFile
  StrCmp $FileResult 1 uninstall_done_${UNINSTALL_ID}
  ; Shell/indexer readers may briefly deny DELETE sharing. Retry only those
  ; exact OS errors, retaining all ownership/reparse checks on every attempt.
  StrCmp $FileOpenError 32 uninstall_wait_${UNINSTALL_ID} uninstall_preserve_${UNINSTALL_ID}
  uninstall_wait_${UNINSTALL_ID}:
    IntOp $FileRetry $FileRetry + 1
    IntCmp $FileRetry 6 uninstall_preserve_${UNINSTALL_ID} 0 uninstall_preserve_${UNINSTALL_ID}
    Sleep 100
    Goto uninstall_retry_${UNINSTALL_ID}
  uninstall_preserve_${UNINSTALL_ID}:
    IntOp $Preserved $Preserved + 1
    DetailPrint "保留已修改、非本软件或正在使用的文件：$FilePath"
  uninstall_done_${UNINSTALL_ID}:
  DeleteRegValue HKCU "${OWN_KEY}" "${VALUE}"
  DeleteRegValue HKCU "${OWN_KEY}" "Identity:${VALUE}"
!macroend

!macro RemoveRegValue NAME EXPECTED
  ReadRegStr $0 HKCU "${ARP_KEY}" "${NAME}"
  StrCmp $0 "${EXPECTED}" 0 +2
    DeleteRegValue HKCU "${ARP_KEY}" "${NAME}"
!macroend

Section "Uninstall"
  ; Lock shortcut ancestors too; even a relocated desktop must not be followed
  ; through a junction. Abort the whole operation before deleting any file.
  Push "$DESKTOP"
  Call un.LockChain
  Push "$SMPROGRAMS"
  Call un.LockChain
  !insertmacro UninstallFile "$INSTDIR\compositor.exe" "SHA256:compositor.exe"
  !insertmacro UninstallFile "$INSTDIR\README.md" "SHA256:README.md"
  !insertmacro UninstallFile "$INSTDIR\LICENSES.md" "SHA256:LICENSES.md"
  !insertmacro UninstallFile "$INSTDIR\SKILL.md" "SHA256:SKILL.md"
  !insertmacro UninstallFile "$INSTDIR\USER_AGENT_SKILL.md" "SHA256:USER_AGENT_SKILL.md"
  !insertmacro UninstallFile "$INSTDIR\runtime-imports.json" "SHA256:runtime-imports.json"
  !insertmacro UninstallFile "$INSTDIR\package-files.json" "SHA256:package-files.json"
  !insertmacro UninstallFile "$DESKTOP\${APP_NAME}.lnk" "DesktopShortcut"
  !insertmacro UninstallFile "$SMPROGRAMS\${APP_NAME}.lnk" "StartMenuShortcut"
  !insertmacro UninstallFile "$INSTDIR\uninstall.exe" "SHA256:uninstall.exe"
  !insertmacro RemoveRegValue "DisplayName" "${APP_NAME}"
  !insertmacro RemoveRegValue "DisplayVersion" "${APP_VERSION}"
  !insertmacro RemoveRegValue "Publisher" "Compositor contributors"
  !insertmacro RemoveRegValue "InstallLocation" "$INSTDIR"
  !insertmacro RemoveRegValue "DisplayIcon" "$INSTDIR\compositor.exe,0"
  !insertmacro RemoveRegValue "UninstallString" '$\"$INSTDIR\uninstall.exe$\"'
  !insertmacro RemoveRegValue "QuietUninstallString" '$\"$INSTDIR\uninstall.exe$\" /S'
  ReadRegDWORD $0 HKCU "${ARP_KEY}" "NoModify"
  StrCmp $0 1 0 +2
    DeleteRegValue HKCU "${ARP_KEY}" "NoModify"
  ReadRegDWORD $0 HKCU "${ARP_KEY}" "NoRepair"
  StrCmp $0 1 0 +2
    DeleteRegValue HKCU "${ARP_KEY}" "NoRepair"
  DeleteRegKey /ifempty HKCU "${ARP_KEY}"
  DeleteRegValue HKCU "${OWN_KEY}" "AppId"
  DeleteRegValue HKCU "${OWN_KEY}" "InstallLocation"
  DeleteRegValue HKCU "${OWN_KEY}" "Version"
  DeleteRegKey /ifempty HKCU "${OWN_KEY}"
  SetOutPath "$LOCALAPPDATA"
  Call un.ReleaseLocks
  ; Deliberately remove only the empty application leaf, never its parent.
  RMDir "$INSTDIR"
  DetailPrint "卸载完成。工程、设置及自行添加的文件已保留。"
  SetErrorLevel 0
SectionEnd
