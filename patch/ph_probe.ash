echo PH-START
savebin c:\ph_init_state.bin 0x08ef6100 l 0x10
sleep 6
writel 0x08e755ec 0xe12fff1e
writel 0x97231b4 0x8ef3af4
alarm c:\DXO_FILTERS\lut_gold.bin
writel 0x97231b4 0x92401c0
echo PH-BOOTSTRAP-RETURNED-CHECK-STATE
savebin c:\ph_boot_state.bin 0x08ef6100 l 0x10
savebin c:\ph_tick.bin 0x09cc238c l 0x4
savebin c:\ph_hook_word.bin 0x08ee9c9c l 0x4
savebin c:\ph_ring.bin 0x08e11fd0 l 0x14
echo PH-READY-ROUND-TRIP
savebin c:\ph_p00.bin 0x095d6dad l 0x4
savebin c:\ph_f00.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m00.bin 0x09ba53f4 l 0x8
savebin c:\ph_v00.bin 0x095d6daa l 0x4
sleep 5
savebin c:\ph_p01.bin 0x095d6dad l 0x4
savebin c:\ph_f01.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m01.bin 0x09ba53f4 l 0x8
savebin c:\ph_v01.bin 0x095d6daa l 0x4
sleep 5
savebin c:\ph_p02.bin 0x095d6dad l 0x4
savebin c:\ph_f02.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m02.bin 0x09ba53f4 l 0x8
savebin c:\ph_v02.bin 0x095d6daa l 0x4
sleep 5
savebin c:\ph_p03.bin 0x095d6dad l 0x4
savebin c:\ph_f03.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m03.bin 0x09ba53f4 l 0x8
savebin c:\ph_v03.bin 0x095d6daa l 0x4
sleep 5
savebin c:\ph_p04.bin 0x095d6dad l 0x4
savebin c:\ph_f04.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m04.bin 0x09ba53f4 l 0x8
savebin c:\ph_v04.bin 0x095d6daa l 0x4
sleep 5
savebin c:\ph_p05.bin 0x095d6dad l 0x4
savebin c:\ph_f05.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m05.bin 0x09ba53f4 l 0x8
savebin c:\ph_v05.bin 0x095d6daa l 0x4
sleep 5
savebin c:\ph_p06.bin 0x095d6dad l 0x4
savebin c:\ph_f06.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m06.bin 0x09ba53f4 l 0x8
savebin c:\ph_v06.bin 0x095d6daa l 0x4
sleep 5
savebin c:\ph_p07.bin 0x095d6dad l 0x4
savebin c:\ph_f07.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m07.bin 0x09ba53f4 l 0x8
savebin c:\ph_v07.bin 0x095d6daa l 0x4
sleep 5
savebin c:\ph_p08.bin 0x095d6dad l 0x4
savebin c:\ph_f08.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m08.bin 0x09ba53f4 l 0x8
savebin c:\ph_v08.bin 0x095d6daa l 0x4
sleep 5
savebin c:\ph_p09.bin 0x095d6dad l 0x4
savebin c:\ph_f09.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m09.bin 0x09ba53f4 l 0x8
savebin c:\ph_v09.bin 0x095d6daa l 0x4
sleep 5
savebin c:\ph_p10.bin 0x095d6dad l 0x4
savebin c:\ph_f10.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m10.bin 0x09ba53f4 l 0x8
savebin c:\ph_v10.bin 0x095d6daa l 0x4
sleep 5
savebin c:\ph_p11.bin 0x095d6dad l 0x4
savebin c:\ph_f11.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m11.bin 0x09ba53f4 l 0x8
savebin c:\ph_v11.bin 0x095d6daa l 0x4
sleep 5
savebin c:\ph_p12.bin 0x095d6dad l 0x4
savebin c:\ph_f12.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m12.bin 0x09ba53f4 l 0x8
savebin c:\ph_v12.bin 0x095d6daa l 0x4
sleep 5
savebin c:\ph_p13.bin 0x095d6dad l 0x4
savebin c:\ph_f13.bin 0x095d5aa8 l 0xc00
savebin c:\ph_m13.bin 0x09ba53f4 l 0x8
savebin c:\ph_v13.bin 0x095d6daa l 0x4
savebin c:\ph_log_end.bin 0x08ef61f0 l 0x210
savebin c:\ph_slots_end.bin 0x092d72e8 l 0x15680
savebin c:\ph_state_end.bin 0x08ef6000 l 0x220
savebin c:\ph_timing.bin 0x08ef6180 l 0x10
savebin c:\ph_durations.bin 0x08ef6600 l 0x80
echo PH-END
