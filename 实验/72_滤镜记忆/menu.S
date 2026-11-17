.syntax unified
.arch armv7-a
.arm
.section .text,"ax",%progbits
.global menu_page_group, menu_group_dispatch, menu_touch, menu_mode3, menu_fresh
.global menu_half_press, menu_home_commit, menu_vf_commit, menu_vf_request, menu_animation_commit
.global menu_draw_active, menu_draw_flush, menu_code_end
.global menu_busy, menu_displayed, menu_redraws, menu_last_result, menu_vf_pending, menu_state_end

/* 56：保留55十档直出和页记忆，修复预览横滑与返回残影。
   滤镜页是 HOME 上的纯绘制扩展，不借照片格式页、不写 touch_prefs。
   HZ 在分派前把逻辑 OLED 模式置 1；自绘完成须配对恢复 0，否则下一滑早退。
   半按沿真实 S1 继续，先撤销虚拟页，避免 HOME 画面仍挂着第六组页标记。 */
.equ PAGE_ADDR, 0x095D6DAD
.equ OLED_MODE, 0x095D6DAA
.equ STATE, 0x08EF6000
.equ CORE, 0x08EF6100
.equ MODE_LOG, 0x08EF61F8
.equ TICK_PTR, 0x09CC238C
.equ EVENT_LOG, 0x08EF6700   /* 59诊断：环形事件码记录，头16B(count)+32条*8B(tick,event) */

.macro SAVE_ALL
    push {r0-r12, lr}
    mrs r0, cpsr
    push {r0, r1}
.endm
.macro RESTORE_ALL
    pop {r0, r1}
    msr APSR_nzcvqg, r0
    pop {r0-r12, lr}
.endm
.macro LOADPAGE reg
    movw \reg, #0x6dad
    movt \reg, #0x095d
.endm
.macro LOADMODE reg
    movw \reg, #0x6daa
    movt \reg, #0x095d
.endm

menu_page_group:
    SAVE_ALL
    LOADPAGE r0
    ldrb r0, [r0]
    cmp r0, #0x95
    beq .Lph_filter
    RESTORE_ALL
    movw r0, #0x6dad
    b fw_page_resume
.Lph_filter:
    RESTORE_ALL
    mov r9, #5
    mov r7, #1
    b fw_hz_group

menu_group_dispatch:
    SAVE_ALL
    ldr r0, [sp, #8]
    cmp r0, #5
    beq .Lgd_filter
    RESTORE_ALL
    cmp r0, #2
    b fw_group_resume
.Lgd_filter:
    /* 保留进入前原厂 chrome。56C0 是状态落盘而非画图，绝不借调用。 */
    LOADPAGE r0
    mov r1, #0x95
    strb r1, [r0]
    mov r1, #0xffffffff
    movw r0, #0x6000
    movt r0, #0x08ef
    str r1, [r0, #4]
    mov r1, #0
    str r1, [r0, #16]
    bl menu_draw_flush
    mov r0, #0
    bl fw_oled_set_mode
    bl fw_shooting_mode
    str r0, [sp, #8]
    RESTORE_ALL
    b fw_hz_early_return

/* 仅撤销虚拟页，随后原封不动重放被替换的 BL 并续接真实 S1。 */
menu_half_press:
    SAVE_ALL
    LOADPAGE r0
    ldrb r1, [r0]
    cmp r1, #0x95
    bne .Lhp_forward
    mov r1, #0
    strb r1, [r0]
    /* 半按撤销95，保持已验收S1语义。 */
    movw r0, #0x6000
    movt r0, #0x08ef
    mov r1, #0xffffffff
    str r1, [r0, #4]
    mov r1, #0
    str r1, [r0, #16]
.Lhp_forward:
    RESTORE_ALL
    /* 原 BL 的返回地址也须一致；尾跳查询，直接返回真实 08E069D4。 */
    movw lr, #0x69d4
    movt lr, #0x08e0
    b fw_s1_ready

/* 只挂 OsdServer_BackHome 成功收尾；动画超时早退不经过此点。
   原 BL SetMode(0) 的入参、标志和 LR 原样重放。 */
menu_home_commit:
    SAVE_ALL
    LOADPAGE r0
    ldrb r1, [r0]
    cmp r1, #0x95
    bne .Lhome_forward
    /* 保留95为最后菜单记忆；只有半按主动撤销。 */
    movw r0, #0x6000
    movt r0, #0x08ef
    mov r1, #0xffffffff
    str r1, [r0, #4]
    mov r1, #0
    str r1, [r0, #16]
.Lhome_forward:
    RESTORE_ALL
    movw lr, #0xf120
    movt lr, #0x08ec
    b fw_oled_set_mode

/* view_EnterVF 非动画成功提交点：保留95记忆并失效最近绘制缓存。 */
menu_vf_commit:
    SAVE_ALL
    LOADPAGE r0
    ldrb r1, [r0]
    cmp r1, #0x95
    bne .Lvf_forward
    /* 保留95为最后菜单记忆；只有半按主动撤销。 */
    movw r0, #0x6000
    movt r0, #0x08ef
    mov r1, #0xffffffff
    str r1, [r0, #4]
    mov r1, #0
    str r1, [r0, #16]
.Lvf_forward:
    RESTORE_ALL
    strb r0, [r6, #2]
    b fw_vf_resume

/* 只记录原厂进入VF的动画请求；此处不退出滤镜页。
   私有pending不把双向共用B6误当作本次滤镜页的退出完成。 */
menu_vf_request:
    SAVE_ALL
    movw r2, #0x6000
    movt r2, #0x08ef
    mov r1, #0
    str r1, [r2, #16]
    ldr r0, [sp, #8]
    cmp r0, #1
    bne .Lvr_forward
    LOADPAGE r0
    ldrb r0, [r0]
    cmp r0, #0x95
    bne .Lvr_forward
    movw r0, #0x53f4
    movt r0, #0x09ba
    ldr r1, [r0]
    cmp r1, #1
    bne .Lvr_forward
    ldr r1, [r0, #4]
    cmp r1, #0
    beq .Lvr_forward
    mov r1, #1
    str r1, [r2, #16]
.Lvr_forward:
    RESTORE_ALL
    strb r0, [r7, #0xfea]
    b fw_vf_request_resume

/* B6收尾：在原厂清pending和动画之前消费已配对的进入VF请求。
   不改显示模式或95记忆；仅匹配时失效绘制缓存。
   原消息无序号，此配对不构成跨任务原子或全部陈旧消息隔离证明。 */
menu_animation_commit:
    SAVE_ALL
    movw r2, #0x6000
    movt r2, #0x08ef
    ldr r1, [r2, #16]
    cmp r1, #1
    bne .Lac_forward
    mov r1, #0
    str r1, [r2, #16]
    ldr r0, [sp, #8]
    cmp r0, #0
    bne .Lac_forward
    ldr r0, [sp, #40]
    ldrb r1, [r0, #0xfea]
    cmp r1, #1
    bne .Lac_forward
    movw r0, #0x53f4
    movt r0, #0x09ba
    ldr r1, [r0]
    cmp r1, #1
    bne .Lac_forward
    ldr r1, [r0, #4]
    cmp r1, #0
    beq .Lac_forward
    LOADPAGE r0
    ldrb r1, [r0]
    cmp r1, #0x95
    bne .Lac_forward
    /* 自动进入VF后仍记住95，活动判定由display完成。 */
    mov r1, #0xffffffff
    str r1, [r2, #4]
.Lac_forward:
    RESTORE_ALL
    strb r0, [r8, #0xfea]
    b fw_animation_resume

menu_touch:
    SAVE_ALL
    cmp r9, #0x95
    beq .Lmt_filter
    RESTORE_ALL
    cmp r9, #0
    b fw_touch_resume
.Lmt_filter:
    mov r10, #0
    b menu_filter_keys

menu_mode3:
    SAVE_ALL
    LOADPAGE r0
    ldrb r0, [r0]
    cmp r0, #0x95
    beq .Lm3_filter
    RESTORE_ALL
    movw r0, #0x192
    b fw_mode3_resume
.Lm3_filter:
    /* 59诊断：记录 mode3 在滤镜页(0x95)收到的事件码，环形32条 {tick,event} */
    movw r0, #0x238c
    movt r0, #0x09cc
    ldr r1, [r0]
    movw r2, #0x6700
    movt r2, #0x08ef
    ldr r3, [r2]
    and r3, r3, #31
    add r3, r3, lsl #3
    add r2, r2, #16
    add r2, r2, r3
    str r1, [r2, #0]
    str r4, [r2, #4]
    movw r0, #0x6700
    movt r0, #0x08ef
    ldr r1, [r0]
    add r1, r1, #1
    str r1, [r0]
    /* 预览纵滑先让原厂退出VF，绝不在本次手势装表。 */
    movw r0, #0x190
    cmp r4, r0
    beq .Lm3_return_home
    add r0, r0, #1
    cmp r4, r0
    beq .Lm3_return_home
    /* 预览横滑及其它键重放原mode3比较/分派，绝不先切HOME。 */
    RESTORE_ALL
    movw r0, #0x192
    b fw_mode3_resume
.Lm3_return_home:
    RESTORE_ALL
    b fw_touch_original

/* 与原生86..94共享HOME/edit渲染入口；95不访问原生页表。
   6DE3为动画离屏构帧门：非零只合成，不自行提交面板。 */
.global menu_native_renderer
menu_native_renderer:
    SAVE_ALL
    LOADPAGE r0
    ldrb r0, [r0]
    cmp r0, #0x95
    bne .Lnr_forward
    LOADMODE r0
    ldrb r0, [r0]
    cmp r0, #0
    bne .Lnr_forward
    bl fw_oled_get_mode
    cmp r0, #1
    bhi .Lnr_forward
    /* 对齐原生9B28批处理：清旧预览后统一提交，保存并恢复原batch。
       扩为整屏清除，使离屏入口也不依赖动画事先清顶栏。 */
    movw r8, #0x5a6c
    movt r8, #0x095d
    ldr r9, [r8, #0x2c]
    mov r0, #1
    str r0, [r8, #0x2c]
    mov r0, #0
    mov r1, #0
    mov r2, #64
    mov r3, #48
    bl fw_clear
    mov r0, #1
    bl fw_native_arrows
    bl fw_native_chrome
    movw r0, #0x6de3
    movt r0, #0x095d
    ldrb r0, [r0]
    cmp r0, #0
    bne .Lnr_offscreen
    bl menu_draw_active
    bl menu_fresh_body
    b .Lnr_done
.Lnr_offscreen:
    bl menu_draw_active
.Lnr_done:
    str r9, [r8, #0x2c]
    mov r0, #1
    str r0, [sp, #8]
    RESTORE_ALL
    bx lr
.Lnr_forward:
    RESTORE_ALL
    movw r0, #0x6dad
    b fw_native_renderer_resume

.global menu_area_gate
/* 原生area包装只覆盖86..94；为95动画离屏绘制补同等抑制。
   恢复完整上下文后原样重放push，非95及非离屏路径透明。 */
menu_area_gate:
    SAVE_ALL
    LOADPAGE r0
    ldrb r0, [r0]
    cmp r0, #0x95
    bne .Lag_forward
    LOADMODE r0
    ldrb r0, [r0]
    cmp r0, #0
    bne .Lag_forward
    movw r0, #0x6de3
    movt r0, #0x095d
    ldrb r0, [r0]
    cmp r0, #0
    bne .Lag_suppress
    /* 保存batch内的清屏/字形只合成；真实Fresh置flush=1时才放行。 */
    movw r0, #0x5a6c
    movt r0, #0x095d
    ldr r1, [r0, #0x2c]
    cmp r1, #0
    beq .Lag_forward
    ldr r1, [r0, #0x30]
    cmp r1, #0
    bne .Lag_forward
.Lag_suppress:
    RESTORE_ALL
    bx lr
.Lag_forward:
    RESTORE_ALL
    push {r0-r3, r12, lr}
    b fw_area_gate_resume

menu_filter_keys:
    bl fw_shooting_mode
    movw r1, #0x61f8
    movt r1, #0x08ef
    str r0, [r1]
    movw r0, #0x190
    cmp r4, r0
    beq .Lfk_up
    add r0, r0, #1
    cmp r4, r0
    beq .Lfk_down
    add r0, r0, #1
    cmp r4, r0
    beq .Lfk_leave
    add r0, r0, #1
    cmp r4, r0
    beq .Lfk_leave
    RESTORE_ALL
    b fw_touch_original
.Lfk_up:
    movw r0, #0x6100
    movt r0, #0x08ef
    ldr r0, [r0]
    cmp r0, #9
    bhi .Lfk_zero
    add r0, r0, #1
    cmp r0, #10
    moveq r0, #0
    b .Lfk_apply
.Lfk_down:
    movw r0, #0x6100
    movt r0, #0x08ef
    ldr r0, [r0]
    cmp r0, #9
    bhi .Lfk_zero
    subs r0, r0, #1
    bpl .Lfk_apply
    mov r0, #9
    b .Lfk_apply
.Lfk_zero:
    /* 未知态不与启动shell并发装表，显示WAIT直至有效current发布。 */
    bl menu_draw_flush
    RESTORE_ALL
    b fw_touch_consumed
.Lfk_apply:
    bl filter_select
    movw r1, #0x6000
    movt r1, #0x08ef
    str r0, [r1, #12]
    bl menu_draw_flush
    RESTORE_ALL
    b fw_touch_consumed
.Lfk_leave:
    /* 仅菜单模式可到此；预览横滑在mode3入口透明交原厂曝光路径。 */
    RESTORE_ALL
    b fw_touch_original

menu_fresh:
    SAVE_ALL
    LOADPAGE r0
    ldrb r0, [r0]
    cmp r0, #0x95
    bne .Lfr_forward
    LOADMODE r0
    ldrb r0, [r0]
    cmp r0, #0
    bne .Lfr_forward
    /* 先合成当前帧，再执行原厂提交，不能先提交旧空帧后补字。 */
    bl menu_draw_active
    bl menu_fresh_body
    RESTORE_ALL
    bx lr
.Lfr_forward:
    RESTORE_ALL
    b menu_fresh_body

menu_fresh_body:
    push {r4, lr}
    b fw_fresh_resume

menu_draw_flush:
    push {r4, r6, r7, lr}
    mov r7, #1
    bl menu_draw_common
    pop {r4, r6, r7, pc}
menu_draw_active:
    push {r4, r6, r7, lr}
    mov r7, #0
    bl menu_draw_common
    pop {r4, r6, r7, pc}
menu_draw_common:
    SAVE_ALL
    movw r4, #0x6000
    movt r4, #0x08ef
    movw r5, #0x6100
    movt r5, #0x08ef
    LOADPAGE r0
    ldrb r0, [r0]
    cmp r0, #0x95
    bne .Ldc_return
    LOADMODE r0
    ldrb r0, [r0]
    cmp r0, #0
    bne .Ldc_return
    ldr r6, [r5]
    cmp r6, #9
    bhi .Ldc_unknown
    mov r0, #4
    mov r1, #14
    mov r2, #55
    mov r3, #18
    bl fw_clear
    str r6, [r4, #4]
    cmp r6, #0
    beq .Ldc_original
    cmp r6, #1
    beq .Ldc_velvia
    cmp r6, #2
    beq .Ldc_gold
    cmp r6, #3
    beq .Ldc_portra
    cmp r6, #4
    beq .Ldc_hp5
    cmp r6, #5
    beq .Ldc_posi
    cmp r6, #6
    beq .Ldc_hibw
    cmp r6, #7
    beq .Ldc_fuyu
    cmp r6, #8
    beq .Ldc_kyus
    adr r0, .Lname_sato
    mov r1, #19
    b .Ldc_emit
.Ldc_hp5:
    adr r0, .Lname_hp5
    mov r1, #25
    b .Ldc_emit
.Ldc_posi:
    adr r0, .Lname_posi
    mov r1, #11
    b .Ldc_emit
.Ldc_hibw:
    adr r0, .Lname_hibw
    mov r1, #11
    b .Ldc_emit
.Ldc_fuyu:
    adr r0, .Lname_fuyu
    mov r1, #18
    b .Ldc_emit
.Ldc_kyus:
    adr r0, .Lname_kyus
    mov r1, #18
    b .Ldc_emit
.Ldc_gold:
    adr r0, .Lname_gold
    mov r1, #15
    b .Ldc_emit
.Ldc_portra:
    adr r0, .Lname_portra
    mov r1, #18
    b .Ldc_emit
.Ldc_original:
    adr r0, .Lname_original
    mov r1, #16
    b .Ldc_emit
.Ldc_velvia:
    adr r0, .Lname_velvia
    mov r1, #21
.Ldc_emit:
    mov r2, #21
    bl menu_text_at
    mov r0, #1
    str r0, [r4]
    ldr r0, [r4, #8]
    add r0, r0, #1
    str r0, [r4, #8]
    b .Ldc_flush
.Ldc_unknown:
    mov r0, #4
    mov r1, #14
    mov r2, #55
    mov r3, #18
    bl fw_clear
    mov r0, #0xffffffff
    str r0, [r4, #4]
    /* WAIT 不是滤镜名，仅表示有效current尚未发布。 */
    adr r0, .Lname_wait
    mov r1, #23
    mov r2, #21
    bl menu_text_at
.Ldc_flush:
    cmp r7, #0
    beq .Ldc_return
    mov r0, #0
    mov r1, #0
    mov r2, #64
    mov r3, #48
    bl fw_area
.Ldc_return:
    movw r4, #0x6000
    movt r4, #0x08ef
    mov r0, #0
    str r0, [r4]
    RESTORE_ALL
    bx lr
menu_text_at:
    /* r0=字符串, r2=y。63: 自制9x15窄体粗体, 箭头间(5..59)居中 */
    push {r4, lr}
    sub sp, sp, #8
    mov r4, r0
    mov r3, #0
.Lmt_len:
    ldrb r1, [r4, r3]
    cmp r1, #0
    beq .Lmt_calc
    add r3, r3, #1
    b .Lmt_len
.Lmt_calc:
    mov r1, #9
    mul r3, r3, r1
    rsb r3, r3, #54
    cmp r3, #0
    movlt r3, #0
    lsr r3, r3, #1
    add r1, r3, #5
    mov r2, #18
    mov r0, r4
    bl menu_draw_text
    add sp, sp, #8
    pop {r4, pc}

/* r0=字符串, r1=x, r2=y。直接写OLED帧缓冲0x095D5AA8(1字节/像素,64宽) */
menu_draw_text:
    push {r4-r11, lr}
    sub sp, sp, #4
    mov r4, r0
    mov r5, r1
    mov r6, r2
    movw r7, #0x5aa8
    movt r7, #0x095d
    movw r3, #0x8000
.Ldt_ch:
    ldrb r8, [r4]
    cmp r8, #0
    beq .Ldt_done
    sub r8, r8, #0x20
    cmp r8, #0x5f
    bhi .Ldt_next
    adr r9, menu_index
    ldrb r8, [r9, r8]
    cmp r8, #0xff
    beq .Ldt_next
    mov r9, #24
    mul r8, r8, r9
    adr r9, menu_font
    add r9, r9, r8
    mov r10, #0
.Ldt_row:
    lsl r2, r10, #1
    ldrh r11, [r9, r2]
    mov r12, #0
.Ldt_col:
    cmp r11, r3
    blt .Ldt_skip
    add r0, r6, r10
    lsl r0, r0, #6
    add r0, r0, r5
    add r0, r0, r12
    add r0, r7, r0
    mov r1, #0xff
    strb r1, [r0]
.Ldt_skip:
    lsl r11, r11, #1
    mov r11, r11, lsl #16
    mov r11, r11, lsr #16
    add r12, r12, #1
    cmp r12, #9
    blt .Ldt_col
    add r10, r10, #1
    cmp r10, #12
    blt .Ldt_row
.Ldt_next:
    add r5, r5, #9
    add r4, r4, #1
    b .Ldt_ch
.Ldt_done:
    add sp, sp, #4
    pop {r4-r11, lr}
    bx lr
.balign 4
.Lname_original: .asciz "NONE"
.balign 4
.Lname_velvia: .asciz "VELVIA"
.balign 4
.Lname_gold: .asciz "G200"
.balign 4
.Lname_portra: .asciz "PORTRA"
.balign 4
.Lname_hp5: .asciz "HP5"
.balign 4
.Lname_posi: .asciz "R-POSI"
.balign 4
.Lname_hibw: .asciz "R-HIBW"
.balign 4
.Lname_fuyu: .asciz "FUYUNO"
.balign 4
.Lname_kyus: .asciz "KYUSHU"
.balign 4
.Lname_sato: .asciz "SATOBI"
.balign 4
.Lname_wait: .asciz "WAIT"
.balign 4
menu_index:
    .byte 0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0,0xff,0xff
    .byte 1,0xff,2,0xff,0xff,3,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff
    .byte 0xff,4,5,0xff,0xff,6,7,8,9,10,0xff,11,12,0xff,13,14
    .byte 15,0xff,16,17,18,19,20,21,0xff,22,0xff,0xff,0xff,0xff,0xff,0xff
    .byte 0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff
    .byte 0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff
.balign 4
menu_font:
    .short 0x0000,0x0000,0x0000,0xff00,0x7f00,0x0000,0x0000,0x0000
    .short 0x0000,0x0000,0x0000,0x0000,0x0000,0x7e00,0x7e00,0xef00
    .short 0xff00,0xff00,0xff00,0xf700,0xf700,0x7e00,0x7e00,0x0000
    .short 0x0000,0x7f00,0xff00,0x4300,0x0300,0x0700,0x0f00,0x1e00
    .short 0x3c00,0x7f00,0xff00,0x0000,0x0000,0xff00,0xfe00,0xfc00
    .short 0xfe00,0xff00,0x4300,0x0300,0x4300,0xff00,0x7e00,0x0000
    .short 0x0000,0x1c00,0x3e00,0x3e00,0x3e00,0x3e00,0x7f00,0x7f00
    .short 0x7f00,0xe380,0xe380,0x0000,0x0000,0xfe00,0xff00,0xe700
    .short 0xff00,0xfe00,0xff00,0xe700,0xe700,0xff00,0xfe00,0x0000
    .short 0x0000,0x7f00,0x7f00,0x7000,0x7e00,0x7f00,0x7e00,0x7000
    .short 0x7000,0x7f00,0x7f00,0x0000,0x0000,0x7f00,0x7f00,0x7000
    .short 0x7e00,0x7f00,0x7e00,0x7000,0x7000,0x7000,0x7000,0x0000
    .short 0x0000,0x3f00,0x7f00,0xe100,0xe000,0xe300,0xe700,0xe300
    .short 0xe300,0x7f00,0x3f00,0x0000,0x0000,0xe700,0xe700,0xe700
    .short 0xff00,0xff00,0xff00,0xe700,0xe700,0xe700,0xe700,0x0000
    .short 0x0000,0x7f00,0x3e00,0x1c00,0x1c00,0x1c00,0x1c00,0x1c00
    .short 0x1c00,0x3e00,0x7f00,0x0000,0x0000,0xe700,0xee00,0xfe00
    .short 0xfc00,0xf800,0xfc00,0xfe00,0xee00,0xe700,0xe780,0x0000
    .short 0x0000,0xe000,0xe000,0xe000,0xe000,0xe000,0xe000,0xe000
    .short 0xe000,0xff00,0xff00,0x0000,0x0000,0xe700,0xf700,0xf700
    .short 0xff00,0xff00,0xff00,0xff00,0xdf00,0xdf00,0xcf00,0x0000
    .short 0x0000,0x3e00,0x7f00,0xe300,0xe300,0xe300,0xe300,0xe300
    .short 0xe300,0x7f00,0x3e00,0x0000,0x0000,0xff00,0xff00,0xe300
    .short 0xe300,0xff00,0xff00,0xfe00,0xe000,0xe000,0xe000,0x0000
    .short 0x0000,0xff00,0xff80,0xe380,0xe380,0xff80,0xff00,0xff00
    .short 0xe380,0xe380,0xe180,0x0000,0x0000,0x7f00,0xff00,0xe100
    .short 0xf000,0x7e00,0x3f00,0x0f00,0x4300,0xff00,0x7f00,0x0000
    .short 0x0000,0xff80,0x7f00,0x1c00,0x1c00,0x1c00,0x1c00,0x1c00
    .short 0x1c00,0x1c00,0x1c00,0x0000,0x0000,0xe300,0xe300,0xe300
    .short 0xe300,0xe300,0xe300,0xe300,0xe300,0xff00,0x7f00,0x0000
    .short 0x0000,0xe380,0xe380,0x7700,0x7700,0x7700,0x3e00,0x3e00
    .short 0x3e00,0x3e00,0x1c00,0x0000,0x0000,0xc380,0xc380,0xdb80
    .short 0xff00,0xff00,0xff00,0xff00,0xff00,0xff00,0xff00,0x0000
    .short 0x0000,0xe380,0x7700,0x7700,0x3e00,0x3e00,0x1c00,0x1c00
    .short 0x1c00,0x1c00,0x1c00,0x0000
.balign 4
.balign 4
menu_code_end:
.section .menu_state,"aw",%progbits
menu_busy: .word 0
menu_displayed: .word 0xffffffff
menu_redraws: .word 0
menu_last_result: .word 0
menu_vf_pending: .word 0
menu_state_end:
