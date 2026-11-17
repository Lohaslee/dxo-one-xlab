.syntax unified
.arch armv7-a
.arm
.section .text,"ax",%progbits
.global filter_select, filter_bootstrap
.global core_code_end
.extern fw_loader
.extern fw_fopen, fw_fread, fw_fwrite, fw_fclose

/* 72 核心：69 复合定稿 + 滤镜记忆。
   切换成功即把 {magic, idx} 8B 写到 c:\ph_last.bin（best-effort，失败不影响切换）；
   开机 bootstrap 先读回上次 idx，校验 magic/范围后走统一 filter_select 恢复，
   文件缺失/损坏/越界一律回退编译期预装档。不轮询、不依赖关机钩子。
   文件 API 签名经固件反汇编 + ambsh_savebin 调用点确证：
   fopen(r0=path,r1=mode)->fp；fclose(r0=fp)->0；
   fread/fwrite 为七参 (r0=buf, r1=0, r2=size=1, r3=0,
   栈[sp+0]=count_lo, 栈[sp+4]=count_hi, 栈[sp+8]=fp)->n（返回字节数）。
   fp 在第 7 参（栈 arg7），不是寄存器 r2——早期按三参 (buf,count,fp)
   调用会把 fp 当 count、栈垃圾当 fp，真机写 0 字节（见说明.md 第三轮根因）。 */

.equ CORE_STATE, 0x08EF6100   /* +0 current +4 last_result +8 busy +12 generation */
.equ LOG_HDR,    0x08EF61F0   /* +0 count +4 last_select_tick +8 last_mode +12 magic */
.equ LOG_BASE,   0x08EF6200   /* 32 条 x 16B，到 0x08EF6400 */
.equ STR_BASE,   0x08EF6400   /* 十条32B目录路径，0x08EF6540结束 */
.equ EXP_BASE,   0x08EF6540   /* 十组(slot1,slot5)，0x08EF6590结束，不碰6600 */
.equ SLOT1,      0x092DB768
.equ SLOT5,      0x092E84E8
.equ TICK_PTR,   0x09CC238C
.equ DEBOUNCE,   180          /* 只抑制同一次手势抖动，不再吞掉2秒内独立滑动 */
.equ TIMING,     0x08EF6180   /* start/end/elapsed/max，单位ms */
.equ DURATIONS,  0x08EF6600   /* 与请求记录slot相同的32个u32耗时，FFFFFFFF=未完成/未加载 */
.equ SAVE_STAT,  0x08EF6190   /* +0 save_ok +4 save_fail +8 restore_src +12 restore_result +16 read_diag */
.equ LAST_MAGIC, 0x534C4850   /* "PHLS" 小端；ph_last.bin 首字校验 */
.equ LAST_PATH,  0x08EF4F00   /* core 区尾部字符串段：路径\0 "w"\0 "r"\0 */
.equ LAST_MODE_W, 0x08EF4F10
.equ LAST_MODE_R, 0x08EF4F12

.macro MOV32 reg, val
    movw \reg, #:lower16:\val
    movt \reg, #:upper16:\val
.endm

/* 环形写一条 16B 记录。入参：r4=idx r6=tick r7=LOG_HDR r8=flags r10=path(0=未装)。
   只破坏 r0-r3；记录地址同时存进 sp+8 供成功后回填指纹。 */
.macro LOG_ENTRY
    ldr r0, [r7, #0]
    and r1, r0, #31
    MOV32 r2, LOG_BASE
    add r2, r2, r1, lsl #4
    str r2, [sp, #8]
    MOV32 r3, DURATIONS
    mov r0, #0xffffffff
    str r0, [r3, r1, lsl #2]
    ldr r0, [r7, #0]
    str r6, [r2, #0]
    orr r3, r4, r8, lsl #16
    str r3, [r2, #4]
    str r10, [r2, #8]
    MOV32 r3, SLOT1
    add r3, r3, #0x1000        /* ldr 立即偏移上限 0xFFF，先加后读 */
    ldr r3, [r3]
    str r3, [r2, #12]
    add r0, r0, #1
    str r0, [r7, #0]
.endm

filter_select:
    push {r4-r11, lr}
    sub sp, sp, #20            /* 36B push +20B locals => AAPCS 8字节对齐；+16保留 */
    mov r4, r0                 /* 目标 idx */
    mov r8, #0                 /* flags: 0 正常 1 防抖 2 无效 3 busy 4 装表失败 */
    MOV32 r5, TICK_PTR
    ldr r6, [r5]               /* tick */
    MOV32 r7, LOG_HDR
    MOV32 r9, CORE_STATE
    cmp r4, #9
    bhi .Linvalid
    ldr r0, [r9, #8]
    cmp r0, #0
    bne .Lbusy                /* 不得把busy调用误判为防抖，也不得释放他人busy */
    ldr r0, [r7, #4]
    sub r0, r6, r0
    movw r1, #DEBOUNCE
    cmp r0, r1
    blo .Ldebounce
    str r6, [r7, #4]           /* last_select_tick = now */
    /* 59加固：busy 用 ldrex/strex 原子占用，避免快速连切时两个调用同时进入 loader */
    add r1, r9, #8
    mov r0, #1
.Lbusy_lock:
    ldrex r2, [r1]
    cmp r2, #0
    beq .Lbusy_set
    clrex
    b .Lbusy
.Lbusy_set:
    strex r3, r0, [r1]
    cmp r3, #0
    bne .Lbusy_lock
    MOV32 r0, TIMING
    str r6, [r0]

    MOV32 r10, STR_BASE
    add r10, r10, r4, lsl #5   /* 32B / 条：idx0..9，目录路径与指纹分区一致 */
    LOG_ENTRY                  /* 先落一条请求记录，装载卡死也留证 */

    str r10, [sp, #0]
    str r10, [sp, #4]
    mov r0, #0
    mov r1, #2
    mov r2, sp                 /* loader: r2=argv, 取 argv[1] 为路径 */
    bl fw_loader
    str r0, [sp, #12]
    MOV32 r0, TICK_PTR
    ldr r1, [r0]
    sub r2, r1, r6
    MOV32 r0, TIMING
    str r1, [r0, #4]
    str r2, [r0, #8]
    ldr r3, [r0, #12]
    cmp r2, r3
    strhi r2, [r0, #12]
    ldr r0, [sp, #8]
    MOV32 r1, LOG_BASE
    sub r0, r0, r1
    lsr r0, r0, #4
    MOV32 r1, DURATIONS
    str r2, [r1, r0, lsl #2]

    /* 装载后抓两处指纹：slot1+0x1000 与 slot5+0x1000。 */
    MOV32 r0, SLOT1
    add r0, r0, #0x1000
    ldr r0, [r0]
    ldr r1, [sp, #8]
    str r0, [r1, #12]          /* 回填自记录第 4 字 */
    MOV32 r1, SLOT5
    add r1, r1, #0x1000
    ldr r1, [r1]
    MOV32 r2, EXP_BASE
    add r2, r2, r4, lsl #3
    ldr r3, [r2, #0]
    cmp r0, r3
    bne .Lfail
    ldr r3, [r2, #4]
    cmp r1, r3
    bne .Lfail
    ldr r11, [sp, #12]
    cmp r11, #0
    bne .Lfail

    /* 成功：先更新 current / 状态，再释放 busy。失败一律保留原有效名。 */
    str r4, [r9, #0]
    mov r0, #0
    str r0, [r9, #4]
    ldr r0, [r9, #12]
    add r0, r0, #1
    str r0, [r9, #12]
    mov r0, r4
    bl save_last               /* 72：切换即存盘；best-effort，保留 r4-r11 */
    dmb sy
    mov r11, #0
    b .Lcommon

.Lfail:
    mov r8, #4
    ldr r0, [sp, #8]
    ldr r1, [r0, #4]
    orr r1, r1, r8, lsl #16
    str r1, [r0, #4]
    mov r0, #3
    str r0, [r9, #4]
    mov r11, #3
    b .Lcommon
.Linvalid:
    mov r8, #2
    mov r11, #2
    b .Lreject
.Ldebounce:
    mov r8, #1
    mov r11, #1
    b .Lreject
.Lbusy:
    mov r8, #3
    mov r11, #4
    mov r10, #0
    LOG_ENTRY
    /* 被拒绝的调用不得清掉别人持有的 busy（已知非原子锁，43 不依赖它互斥）。 */
    mov r0, r11
    b .Lepilogue
.Lreject:
    mov r10, #0
    LOG_ENTRY
    mov r0, r11
    b .Lepilogue
.Lcommon:
    mov r0, #0
    str r0, [r9, #8]
    mov r0, r11
.Lepilogue:
    add sp, sp, #20
    pop {r4-r11, pc}

/* autoexec通过alarm调用统一装表函数，启动current未知，成功后才发布。
   alarm argv与常驻loader相同，不依赖shell直接改current的假确认。
   72：current 未知时先尝试读 ph_last.bin 恢复上次档，失败回退预装档。 */
filter_bootstrap:
    push {r4, lr}
    /* 用户已成功选表时不再覆盖；未知态仍通过统一核心验证后发布。 */
    MOV32 r0, CORE_STATE
    ldr r0, [r0]
    cmp r0, #9
    bhi .Lboot_try_last
    mov r0, #0
    pop {r4, pc}
.Lboot_try_last:
    /* 防抖基准回拨 DEBOUNCE：开机早期 tick<DEBOUNCE 时清零仍会被防抖吞，
       回拨后差值恰=DEBOUNCE（u32 回绕对极早 tick 同样成立） */
    MOV32 r1, TICK_PTR
    ldr r2, [r1]
    sub r2, r2, #DEBOUNCE
    MOV32 r1, LOG_HDR
    str r2, [r1, #4]
    bl read_last               /* r0 = 上次 idx（magic 已校验），失败 = -1 */
    cmn r0, #1
    beq .Lboot_preload
    cmp r0, #9
    bhi .Lboot_preload         /* 越界（清单收缩/文件损坏）回退预装档 */
    mov r4, r0
    bl filter_select
    MOV32 r1, SAVE_STAT
    mov r2, #1                 /* restore_src = 1 记忆文件 */
    str r2, [r1, #8]
    str r0, [r1, #12]
    b .Lboot_return
.Lboot_preload:
    mov r0, #2
    bl filter_select
    MOV32 r1, SAVE_STAT
    mov r2, #0                 /* restore_src = 0 预装档 */
    str r2, [r1, #8]
    str r0, [r1, #12]
.Lboot_return:
    pop {r4, pc}

/* 72：保存 {LAST_MAGIC, idx} 8B 到 c:\ph_last.bin。入参 r0=idx。
   best-effort：任何失败只计 SAVE_STAT，不影响切换结果。保留 r4-r11。 */
save_last:
    push {r4-r6, lr}
    sub sp, sp, #24            /* 16B push + 24B(栈参12+buf8+pad4) => 8字节对齐 */
    mov r4, r0
    MOV32 r0, LAST_MAGIC
    str r0, [sp, #12]          /* buf[0] = magic */
    str r4, [sp, #16]          /* buf[4] = idx */
    MOV32 r0, LAST_PATH
    MOV32 r1, LAST_MODE_W
    bl fw_fopen
    movs r5, r0
    beq .Lsave_fail
    /* fw_fwrite(buf,0,1,0,count_lo,count_hi,fp)：fp 须落栈 arg7[sp+8] */
    mov r0, #8
    str r0, [sp, #0]           /* arg5 count_lo = 8 */
    mov r0, #0
    str r0, [sp, #4]           /* arg6 count_hi = 0 */
    str r5, [sp, #8]           /* arg7 fp */
    add r0, sp, #12            /* r0 = buf */
    mov r1, #0
    mov r2, #1                 /* size = 1 */
    mov r3, #0
    bl fw_fwrite
    mov r6, r0
    mov r0, r5
    bl fw_fclose
    cmp r6, #8
    bne .Lsave_fail
    MOV32 r1, SAVE_STAT
    ldr r0, [r1, #0]
    add r0, r0, #1
    str r0, [r1, #0]
    mov r0, #0
    b .Lsave_out
.Lsave_fail:
    MOV32 r1, SAVE_STAT
    ldr r0, [r1, #4]
    add r0, r0, #1
    str r0, [r1, #4]
    mov r0, #1
.Lsave_out:
    add sp, sp, #24
    pop {r4-r6, pc}

/* 72：读 c:\ph_last.bin 首 8B，校验 magic。返回 r0=idx，失败=-1。
   read_diag 记诊断码：0 成功 1 无文件 2 短读 3 坏magic。保留 r4-r11。 */
read_last:
    push {r4-r6, lr}
    sub sp, sp, #24            /* 栈参12+buf8+pad4，8字节对齐 */
    MOV32 r0, LAST_PATH
    MOV32 r1, LAST_MODE_R
    bl fw_fopen
    movs r5, r0
    beq .Lread_nofile
    /* fw_fread(buf,0,1,0,count_lo,count_hi,fp)：fp 须落栈 arg7[sp+8] */
    mov r0, #8
    str r0, [sp, #0]           /* arg5 count_lo = 8 */
    mov r0, #0
    str r0, [sp, #4]           /* arg6 count_hi = 0 */
    str r5, [sp, #8]           /* arg7 fp */
    add r0, sp, #12            /* r0 = buf */
    mov r1, #0
    mov r2, #1                 /* size = 1 */
    mov r3, #0
    bl fw_fread
    mov r6, r0
    mov r0, r5
    bl fw_fclose
    cmp r6, #8
    bne .Lread_short
    ldr r0, [sp, #12]          /* buf[0] = magic */
    MOV32 r1, LAST_MAGIC
    cmp r0, r1
    bne .Lread_badmagic
    ldr r4, [sp, #16]          /* buf[4] = idx */
    mov r0, #0
    bl .Lread_stat
    mov r0, r4
    b .Lread_out
.Lread_nofile:
    mov r0, #1
    bl .Lread_stat
    mvn r0, #0
    b .Lread_out
.Lread_short:
    mov r0, #2
    bl .Lread_stat
    mvn r0, #0
    b .Lread_out
.Lread_badmagic:
    mov r0, #3
    bl .Lread_stat
    mvn r0, #0
.Lread_out:
    add sp, sp, #24
    pop {r4-r6, pc}
.Lread_stat:
    MOV32 r1, SAVE_STAT
    str r0, [r1, #16]
    bx lr

.balign 4
core_code_end: