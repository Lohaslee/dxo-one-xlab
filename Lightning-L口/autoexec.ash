rm c:\console_debug.txt
t dxo console 8
echo PH-SESSION-START
cd c:\dxot
pwd
writeb 0x95d6dad 0
c:\dxot\m01.ash
c:\dxot\m02.ash
writel 0x97231b4 0x8ee7478
alarm c:\dxot\a.bin
writel 0x97231b4 0x8ee7080
alarm c:\touch_prefs.bin 0x8ee986c
writel 0x97231b4 0x8ee7478
alarm c:\dxot\b.bin
writel 0x97231b4 0x8ee7478
alarm c:\ph_menu.bin
writel 0x97231b4 0x92401c0
echo PH-MENU-LOADED
echo PH-SUB-START
c:\ph_probe.ash
echo PH-SUB-END
savebin c:\PH_DIAG\ph_done.bin 0x08c00000 l 0x4
