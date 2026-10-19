/* 自有合成 bytes 的纯合同测试；不读屏幕、进程、文件、系统时钟或用户内容。 */
#include "pixel_oracle.h"

#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static unsigned int checks;
static unsigned int cases;
static size_t observed_scalar_bytes;
static size_t peak_owned_pixel_bytes;

/* 每个断言只输出固定标签；失败立即结束，不能产生一个继续执行的假通过摘要。 */
static void require(bool value, const char *label) {
    checks += 1U;
    if (!value) {
        /* 标签由本测试源码给定，不把像素或识别文字写入诊断。 */
        fprintf(stderr, "pixel oracle assertion failed: %s\n", label);
        exit(EXIT_FAILURE);
    }
}

/* 小图包含行 padding 和非零 ROI 原点，避免仅测试紧凑的整个 buffer。 */
typedef struct {
    uint8_t old_bytes[160];
    uint8_t new_bytes[160];
    uint8_t current[160];
    OmniPixelConfig config;
} Fixture;

/* 所有 fixture 独立初始化，不共享可变 owner 或借用。 */
static Fixture fixture(void) {
    Fixture f = {0};
    memset(f.old_bytes, 0x55, sizeof(f.old_bytes));
    memcpy(f.new_bytes, f.old_bytes, sizeof(f.old_bytes));
    f.new_bytes[1U * 40U + 3U * 4U] = 0xaa;
    memcpy(f.current, f.old_bytes, sizeof(f.current));
    f.config = (OmniPixelConfig){
        .run_id = 17,
        .start_ticks = 1000,
        .required_baseline_frames = 1,
        .frame_limit = OMNI_PIXEL_MAX_FRAMES,
        .roi = {2, 1, 4, 2},
    };
    return f;
}

/* 借用完整行范围，padding 不属于实际 ROI 像素。 */
static OmniPixelImage image(const uint8_t *bytes) {
    return (OmniPixelImage){bytes, 160, 8, 4, 40};
}

/* 成功构造必须报告实际三份像素容量与固定 scalar 容量。 */
static OmniPixelOracle *create(Fixture *f) {
    OmniPixelStatus status = OMNI_PIXEL_ALLOCATION_FAILED;
    OmniPixelOracle *oracle = omni_pixel_oracle_create(f->config, image(f->old_bytes), image(f->new_bytes), &status);
    require(oracle != NULL && status == OMNI_PIXEL_OK, "fixture owner construction");
    require(omni_pixel_oracle_owned_pixel_bytes(oracle) == 4U * 2U * 4U * 3U, "three actual compact allocations counted");
    require(omni_pixel_oracle_owned_scalar_bytes(oracle) >= sizeof(OmniPixelRecord) * OMNI_PIXEL_MAX_FRAMES,
            "all 600 scalar slots included");
    observed_scalar_bytes = omni_pixel_oracle_owned_scalar_bytes(oracle);
    if (omni_pixel_oracle_owned_pixel_bytes(oracle) > peak_owned_pixel_bytes) {
        /* 摘要使用实际 owner 计数，不把固定上限伪装成测得分配。 */
        peak_owned_pixel_bytes = omni_pixel_oracle_owned_pixel_bytes(oracle);
    }
    return oracle;
}

/* 每个 frame 使用显式合成整数 ticks；它们不被声明为真实 Mach/SCK 时间。 */
static OmniPixelFrame frame(uint32_t index) {
    return (OmniPixelFrame){17, index, 1000U + (uint64_t)index * 16667U,
                            1100U + (uint64_t)index * 16667U};
}

/* 正常输入辅助函数同时验证输出确实属于当前帧，而不是上一帧缓存。 */
static OmniPixelRecord observe(OmniPixelOracle *oracle, uint32_t index, const uint8_t *bytes) {
    OmniPixelRecord record = {0};
    require(omni_pixel_oracle_observe(oracle, frame(index), image(bytes), &record) == OMNI_PIXEL_OK,
            "admit one explicit synthetic frame");
    require(record.frame.index == index && record.frame.display_ticks == frame(index).display_ticks,
            "record retains this frame identity");
    return record;
}

/* 正常归还后销毁必须清空调用方 owner，不能只清空一个计数。 */
static void destroy(OmniPixelOracle **oracle) {
    require(omni_pixel_oracle_destroy(oracle) == OMNI_PIXEL_OK, "owned cleanup succeeds");
    require(*oracle == NULL, "owned cleanup empties caller slot");
}

/* 已完成审计与归还是两个阶段，测试不把外部 bool 当真实 Vision 执行证明。 */
static void finish_audit(OmniPixelOracle *oracle, OmniPixelAuditBinding binding, OmniPixelStatus expected) {
    require(omni_pixel_oracle_audit_complete(oracle, binding, true, true) == expected,
            "synthetic audit returns expected original failure or success");
    require(omni_pixel_oracle_audit_release(oracle, binding) == OMNI_PIXEL_OK, "audit loan explicitly returned");
}

/* 正常观测没有首错时，合成审计与显式归还共同结束候选工作。 */
static void pass_audit(OmniPixelOracle *oracle, OmniPixelAuditBinding binding) {
    finish_audit(oracle, binding, OMNI_PIXEL_OK);
}

/* 构造失败不能留下 owner，具体拒绝原因必须保留。 */
static void rejected(OmniPixelConfig config, OmniPixelImage old, OmniPixelImage new_image, OmniPixelStatus expected) {
    OmniPixelStatus status = OMNI_PIXEL_OK;
    OmniPixelOracle *oracle = omni_pixel_oracle_create(config, old, new_image, &status);
    require(oracle == NULL && status == expected, "invalid construction rejects exact boundary");
}

/* 独立像素反例覆盖完整 ROI、四通道、行 padding、源拷贝与 UNKNOWN。 */
static void exact_pixels(void) {
    cases += 1U;
    Fixture f = fixture();
    OmniPixelOracle *oracle = create(&f);
    OmniPixelState state = OMNI_PIXEL_UNKNOWN;
    require(omni_pixel_oracle_classify(oracle, image(f.old_bytes), &state) == OMNI_PIXEL_OK && state == OMNI_PIXEL_OLD,
            "exact OLD template");
    require(omni_pixel_oracle_classify(oracle, image(f.new_bytes), &state) == OMNI_PIXEL_OK && state == OMNI_PIXEL_NEW,
            "exact NEW template");
    for (size_t y = 1; y < 3; ++y) {
        /* 翻转 ROI 任意一位都不能被近似匹配接受，包含 alpha 字节。 */
        for (size_t x = 2; x < 6; ++x) {
            for (size_t channel = 0; channel < 4; ++channel) {
                for (unsigned int bit = 0; bit < 8; ++bit) {
                    memcpy(f.current, f.new_bytes, sizeof(f.current));
                    f.current[y * 40U + x * 4U + channel] ^= (uint8_t)(1U << bit);
                    require(omni_pixel_oracle_classify(oracle, image(f.current), &state) == OMNI_PIXEL_OK &&
                            state == OMNI_PIXEL_UNKNOWN, "every one-bit near match is UNKNOWN");
                }
            }
        }
    }
    memcpy(f.current, f.new_bytes, sizeof(f.current));
    f.current[0] ^= 1U;
    f.current[39] ^= 1U;
    require(omni_pixel_oracle_classify(oracle, image(f.current), &state) == OMNI_PIXEL_OK && state == OMNI_PIXEL_NEW,
            "outside ROI and padding are not invented glyphs");
    memcpy(f.current, f.old_bytes, sizeof(f.current));
    memset(f.old_bytes, 0, sizeof(f.old_bytes));
    memset(f.new_bytes, 0, sizeof(f.new_bytes));
    require(omni_pixel_oracle_classify(oracle, image(f.current), &state) == OMNI_PIXEL_OK && state == OMNI_PIXEL_OLD,
            "template source mutation cannot change owned reference");
    destroy(&oracle);
}

/* 所有尺寸和乘法反例都在读源 bytes 前拒绝，sanitizer 同时检查越界。 */
static void geometry_and_configuration(void) {
    cases += 1U;
    Fixture f = fixture();
    OmniPixelImage old = image(f.old_bytes), new_image = image(f.new_bytes);
    OmniPixelConfig bad = f.config;
    bad.run_id = 0;
    rejected(bad, old, new_image, OMNI_PIXEL_INVALID_ARGUMENT);
    bad = f.config;
    bad.start_ticks = 0;
    rejected(bad, old, new_image, OMNI_PIXEL_INVALID_ARGUMENT);
    for (unsigned int kind = 0; kind < 4; ++kind) {
        /* 四项有限计数的上下边界必须独立拒绝，不能被另一个非法字段遮蔽。 */
        bad = f.config;
        if (kind < 2U) {
            /* baseline 只接受 1..10。 */
            bad.required_baseline_frames = kind == 0 ? 0 : 11;
        } else {
            /* 帧预算只接受 1..600。 */
            bad.frame_limit = kind == 2U ? 0 : 601;
        }
        rejected(bad, old, new_image, OMNI_PIXEL_INVALID_ARGUMENT);
    }
    bad = f.config;
    bad.roi = (OmniPixelRect){SIZE_MAX, 0, 1, 1};
    rejected(bad, old, new_image, OMNI_PIXEL_INVALID_GEOMETRY);
    bad = f.config;
    bad.roi.width = 0;
    rejected(bad, old, new_image, OMNI_PIXEL_INVALID_GEOMETRY);
    for (unsigned int kind = 0; kind < 7; ++kind) {
        /* 不可信布局不能通过巨大 byte_count 或 padding 假装实际可读。 */
        OmniPixelImage broken = old;
        switch (kind) {
            case 0: /* nil bytes 没有可读取的源。 */
                broken.bytes = NULL;
                break;
            case 1: /* 最后活动行不足一个字节也失败。 */
                broken.byte_count = 151;
                break;
            case 2: /* stride 不能短于完整活动行。 */
                broken.bytes_per_row = 31;
                break;
            case 3: /* 原始宽度乘四溢出在运算之前拒绝。 */
                broken.width = SIZE_MAX;
                break;
            case 4: /* 行跨度累计溢出不能生成指针。 */
                broken.bytes_per_row = SIZE_MAX;
                break;
            case 5: /* 源高变化不能沿用参考几何。 */
                broken.height = 3;
                break;
            default: /* 空图像不代表空白 OLD。 */
                broken.height = 0;
                break;
        }
        rejected(f.config, broken, new_image, OMNI_PIXEL_INVALID_GEOMETRY);
    }
    rejected(f.config, old, old, OMNI_PIXEL_IDENTICAL_TEMPLATES);
    OmniPixelOracle *oracle = create(&f);
    OmniPixelState untouched = OMNI_PIXEL_NEW;
    old.byte_count = 151;
    require(omni_pixel_oracle_classify(oracle, old, &untouched) == OMNI_PIXEL_INVALID_GEOMETRY && untouched == OMNI_PIXEL_NEW,
            "invalid stateless input does not write a fake classification");
    destroy(&oracle);
}

/* mask 允许固定共同像素变化，但不能跨出 ROI 或遮住哪怕一个 OLD/NEW 差异字节。 */
static void fixed_mask(void) {
    cases += 1U;
    Fixture f = fixture();
    f.config.mask = (OmniPixelMask){true, {0, 1, 1, 1}};
    OmniPixelOracle *oracle = create(&f);
    memcpy(f.current, f.new_bytes, sizeof(f.current));
    f.current[2U * 40U + 2U * 4U] ^= 0xffU;
    OmniPixelState state = OMNI_PIXEL_UNKNOWN;
    require(omni_pixel_oracle_classify(oracle, image(f.current), &state) == OMNI_PIXEL_OK && state == OMNI_PIXEL_NEW,
            "only declared common pixel may vary");
    f.current[2U * 40U + 3U * 4U] ^= 1U;
    require(omni_pixel_oracle_classify(oracle, image(f.current), &state) == OMNI_PIXEL_OK && state == OMNI_PIXEL_UNKNOWN,
            "adjacent unmasked near-match still rejected");
    destroy(&oracle);
    f.config.mask.rect = (OmniPixelRect){1, 0, 1, 1};
    rejected(f.config, image(f.old_bytes), image(f.new_bytes), OMNI_PIXEL_MASK_HIDES_DIFFERENCE);
    f.config.mask.rect = (OmniPixelRect){3, 0, 2, 1};
    rejected(f.config, image(f.old_bytes), image(f.new_bytes), OMNI_PIXEL_INVALID_GEOMETRY);
    f.config.mask = (OmniPixelMask){false, {0, 0, 1, 1}};
    rejected(f.config, image(f.old_bytes), image(f.new_bytes), OMNI_PIXEL_INVALID_GEOMETRY);
}

/* 三份真实像素不能借“单 crop 256 KiB”规避总预算。 */
static void total_pixel_budget(void) {
    cases += 1U;
    size_t bytes = 256U * 86U * 4U;
    uint8_t *old_bytes = calloc(1U, bytes), *new_bytes = calloc(1U, bytes);
    require(old_bytes != NULL && new_bytes != NULL, "owned budget fixture allocation");
    new_bytes[0] = 1;
    OmniPixelImage old = {old_bytes, bytes, 256, 86, 1024};
    OmniPixelImage new_image = {new_bytes, bytes, 256, 86, 1024};
    Fixture f = fixture();
    f.config.roi = (OmniPixelRect){0, 0, 256, 85};
    OmniPixelStatus status = OMNI_PIXEL_OK;
    OmniPixelOracle *oracle = omni_pixel_oracle_create(f.config, old, new_image, &status);
    require(oracle != NULL && status == OMNI_PIXEL_OK, "largest selected safe three-image budget");
    require(omni_pixel_oracle_owned_pixel_bytes(oracle) == 261120U, "count all 256 by 85 buffers");
    peak_owned_pixel_bytes = omni_pixel_oracle_owned_pixel_bytes(oracle);
    destroy(&oracle);
    f.config.roi.height = 86;
    rejected(f.config, old, new_image, OMNI_PIXEL_BUDGET_EXCEEDED);
    free(old_bytes);
    free(new_bytes);
}

/* UNKNOWN 打断 baseline；后来成功只证明已审计 witness，不证明之前没有 NEW。 */
static void unknown_and_positive_baseline(void) {
    cases += 1U;
    Fixture f = fixture();
    f.config.required_baseline_frames = 2;
    OmniPixelOracle *oracle = create(&f);
    require(!observe(oracle, 1, f.old_bytes).baseline_ready, "one OLD insufficient for two-frame baseline");
    f.current[1U * 40U + 2U * 4U] ^= 1U;
    require(observe(oracle, 2, f.current).state == OMNI_PIXEL_UNKNOWN, "unknown recorded explicitly");
    require(!observe(oracle, 3, f.old_bytes).baseline_ready, "UNKNOWN resets consecutive baseline");
    require(observe(oracle, 4, f.old_bytes).baseline_ready, "two later positive OLD frames establish baseline");
    require(observe(oracle, 5, f.new_bytes).earliest_observed_candidate, "first later NEW creates witness candidate");
    OmniPixelAuditBinding binding = {0};
    OmniPixelImage candidate = {0};
    require(omni_pixel_oracle_audit_begin(oracle, &binding, &candidate) == OMNI_PIXEL_OK, "begin one candidate audit");
    require(candidate.byte_count == 32U && candidate.width == 4U && candidate.height == 2U,
            "audit receives compact ROI only");
    require(candidate.bytes[4] == 0xaaU && binding.frame_index == 5 && binding.display_ticks == frame(5).display_ticks,
            "audit bytes and identity refer to the same NEW frame");
    pass_audit(oracle, binding);
    OmniPixelFooter footer = omni_pixel_oracle_finish(oracle);
    require(footer.witness_valid && !footer.pre_candidate_frames_all_old && !footer.timeline_complete_for_classification,
            "late witness does not erase UNKNOWN history");
    require(footer.unknown_frames == 1 && footer.unknown_before_candidate == 1, "unknown census is exact");
    require(!footer.latency_acceptance && !footer.capture_completeness_verified, "no latency or capture completeness claim");
    require(footer.owned_pixel_bytes_at_finish == 0 && omni_pixel_oracle_owned_pixel_bytes(oracle) == 0,
            "finish frees all template and candidate capacity");
    destroy(&oracle);
}

/* 600 帧重放期间一直不完成审计，证明分类进度不依赖 OCR 工作完成或覆盖旧帧。 */
static void delayed_audit_600_frames(bool overflow) {
    cases += 1U;
    Fixture f = fixture();
    OmniPixelOracle *oracle = create(&f);
    (void)observe(oracle, 1, f.old_bytes);
    (void)observe(oracle, 2, f.new_bytes);
    OmniPixelAuditBinding binding = {0};
    OmniPixelImage candidate = {0};
    require(omni_pixel_oracle_audit_begin(oracle, &binding, &candidate) == OMNI_PIXEL_OK, "start deferred audit");
    uint8_t candidate_copy[32];
    memcpy(candidate_copy, candidate.bytes, sizeof(candidate_copy));
    for (uint32_t index = 3; index <= OMNI_PIXEL_MAX_FRAMES; ++index) {
        /* 反复 NEW→OLD→NEW 及候选后 UNKNOWN，不能把候选移动到审计完成附近。 */
        memcpy(f.current, index % 2U == 0 ? f.new_bytes : f.old_bytes, sizeof(f.current));
        if (index == 99) {
            /* 候选之后的 UNKNOWN 不影响此前全 OLD 事实，但保留整条分类的不确定性。 */
            f.current[1U * 40U + 2U * 4U] ^= 1U;
        }
        require(!observe(oracle, index, f.current).earliest_observed_candidate, "no later frame replaces candidate");
        require(memcmp(candidate.bytes, candidate_copy, sizeof(candidate_copy)) == 0, "candidate snapshot immutable during all later callbacks");
    }
    OmniPixelRecord first = {0}, last = {0};
    require(omni_pixel_oracle_record(oracle, 0, &first) && first.frame.index == 1, "oldest scalar record not overwritten");
    require(omni_pixel_oracle_record(oracle, 599, &last) && last.frame.index == 600, "last bounded slot is exact");
    require(!omni_pixel_oracle_record(oracle, 600, &last) && last.frame.index == 600, "out of range preserves output");
    pass_audit(oracle, binding);
    if (overflow) {
        /* 即便候选已经审计通过，超预算下一帧也必须使最终 witness 失败。 */
        OmniPixelRecord ignored = {0};
        require(omni_pixel_oracle_observe(oracle, frame(601), image(f.new_bytes), &ignored) == OMNI_PIXEL_FRAME_OVERFLOW,
                "601st frame fails instead of overwriting");
    }
    OmniPixelFooter footer = omni_pixel_oracle_finish(oracle);
    require(footer.witness_valid == !overflow && footer.recorded_frames == 600, "bounded replay final validity");
    require(footer.earliest_observed.frame_index == 2 && footer.earliest_observed.display_ticks == frame(2).display_ticks,
            "audit completion never drifts original display ticks");
    require(footer.pre_candidate_frames_all_old && !footer.timeline_complete_for_classification,
            "candidate-after OLD and later UNKNOWN are separate facts");
    require(!footer.latency_acceptance && !footer.capture_completeness_verified, "pure replay is no capture or paint acceptance");
    destroy(&oracle);
}

/* 所有时序错误都在写记录前失败，原始候选和记录不受污染。 */
static void ordering_failures(void) {
    cases += 1U;
    for (unsigned int kind = 0; kind < 7; ++kind) {
        /* 每个反例使用新 owner，避免首错掩盖另一个实际边界。 */
        Fixture f = fixture();
        OmniPixelOracle *oracle = create(&f);
        (void)observe(oracle, 1, f.old_bytes);
        OmniPixelFrame bad = frame(2);
        switch (kind) {
            case 0: /* 不同 run 的帧不能进入当前序列。 */
                bad.run_id += 1U;
                break;
            case 1: /* 重复帧 index 不能被当作新帧。 */
                bad.index = 1;
                break;
            case 2: /* 缺少已传入序列的 index 必须显式失败。 */
                bad.index = 3;
                break;
            case 3: /* 重复 display tick 不是新呈现证据。 */
                bad.display_ticks = frame(1).display_ticks;
                break;
            case 4: /* 倒退到起点之前同样失败。 */
                bad.display_ticks = 999;
                break;
            case 5: /* callback 时钟倒退不能被 OCR 时间掩盖。 */
                bad.callback_absolute_ticks = frame(1).callback_absolute_ticks;
                break;
            default: /* callback 早于其声明的 display 是不可信时钟输入。 */
                bad.callback_absolute_ticks = bad.display_ticks - 1U;
                break;
        }
        OmniPixelRecord record = {0};
        require(omni_pixel_oracle_observe(oracle, bad, image(f.new_bytes), &record) == OMNI_PIXEL_FRAME_ORDER,
                "frame order rejection");
        require(omni_pixel_oracle_observe(oracle, frame(2), image(f.new_bytes), &record) == OMNI_PIXEL_FRAME_ORDER,
                "later corrected frame cannot erase first failure");
        OmniPixelFooter footer = omni_pixel_oracle_finish(oracle);
        require(!footer.witness_valid && footer.recorded_frames == 1 && footer.failure == OMNI_PIXEL_FRAME_ORDER,
                "failed time input never reaches a valid footer");
        destroy(&oracle);
    }
}

/* NEW 提前存在与只有 OLD/UNKNOWN 两种无候选情形都不能产生证据。 */
static void preexisting_or_missing(void) {
    cases += 1U;
    Fixture f = fixture();
    OmniPixelOracle *oracle = create(&f);
    OmniPixelRecord record = {0};
    require(omni_pixel_oracle_observe(oracle, frame(1), image(f.new_bytes), &record) == OMNI_PIXEL_PREEXISTING_NEW,
            "NEW before positive baseline rejected");
    require(omni_pixel_oracle_finish(oracle).failure == OMNI_PIXEL_PREEXISTING_NEW, "preexisting failure retained");
    destroy(&oracle);
    oracle = create(&f);
    (void)observe(oracle, 1, f.old_bytes);
    OmniPixelFooter footer = omni_pixel_oracle_finish(oracle);
    require(!footer.witness_valid && !footer.candidate_present && footer.failure == OMNI_PIXEL_NO_WITNESS,
            "positive baseline alone is not a NEW witness");
    destroy(&oracle);
}

/* 不同 run/index/ticks 的审计结果无法取得真实候选的权限。 */
static void mismatched_audits(void) {
    cases += 1U;
    for (unsigned int kind = 0; kind < 3; ++kind) {
        /* 三个身份分量分别变化，避免只保护最容易检查的 run。 */
        Fixture f = fixture();
        OmniPixelOracle *oracle = create(&f);
        (void)observe(oracle, 1, f.old_bytes);
        (void)observe(oracle, 2, f.new_bytes);
        OmniPixelAuditBinding binding = {0};
        OmniPixelImage candidate = {0};
        require(omni_pixel_oracle_audit_begin(oracle, &binding, &candidate) == OMNI_PIXEL_OK, "begin identity test audit");
        OmniPixelAuditBinding bad = binding;
        if (kind == 0) {
            /* 另一场测量的同一帧号也不能匹配。 */
            bad.run_id += 1U;
        } else if (kind == 1) {
            /* 同 run 的较晚帧不能回填最早帧。 */
            bad.frame_index += 1U;
        } else {
            /* 只有 tick 改变也属于不同候选。 */
            bad.display_ticks += 1U;
        }
        require(omni_pixel_oracle_audit_complete(oracle, bad, true, true) == OMNI_PIXEL_AUDIT_IDENTITY,
                "mismatched receipt fails closed");
        require(omni_pixel_oracle_audit_release(oracle, bad) == OMNI_PIXEL_AUDIT_IDENTITY,
                "mismatched receipt cannot release actual loan");
        finish_audit(oracle, binding, OMNI_PIXEL_AUDIT_IDENTITY);
        OmniPixelFooter footer = omni_pixel_oracle_finish(oracle);
        require(!footer.witness_valid && footer.failure == OMNI_PIXEL_AUDIT_IDENTITY &&
                footer.earliest_observed.display_ticks == frame(2).display_ticks &&
                footer.audit_complete && footer.audit_passed && footer.audit_loan_released,
                "later correct receipt only cleans up without repairing mismatch");
        destroy(&oracle);
    }
}

/* 内容拒绝和真实释放声明失败都不可被第二次成功覆盖。 */
static void rejected_and_duplicate_audits(void) {
    cases += 1U;
    for (unsigned int kind = 0; kind < 3; ++kind) {
        /* 只在第三组先给成功，再注入非法重复回执。 */
        Fixture f = fixture();
        OmniPixelOracle *oracle = create(&f);
        (void)observe(oracle, 1, f.old_bytes);
        (void)observe(oracle, 2, f.new_bytes);
        OmniPixelAuditBinding binding = {0};
        OmniPixelImage candidate = {0};
        require(omni_pixel_oracle_audit_begin(oracle, &binding, &candidate) == OMNI_PIXEL_OK, "begin rejection test audit");
        OmniPixelStatus expected = kind == 2 ? OMNI_PIXEL_OK : OMNI_PIXEL_AUDIT_REJECTED;
        require(omni_pixel_oracle_audit_complete(oracle, binding, kind != 0, kind != 1) == expected,
                "content and release receipts handled independently");
        require(omni_pixel_oracle_audit_complete(oracle, binding, true, true) == OMNI_PIXEL_AUDIT_STATE,
                "second result is not another chance to pass");
        require(omni_pixel_oracle_audit_release(oracle, binding) == OMNI_PIXEL_OK, "failed audit loan still cleaned");
        OmniPixelFooter footer = omni_pixel_oracle_finish(oracle);
        require(!footer.witness_valid && footer.failure == (kind == 2 ? OMNI_PIXEL_AUDIT_STATE : OMNI_PIXEL_AUDIT_REJECTED),
                "first audit failure survives later calls");
        destroy(&oracle);
    }
}

/* 提前 finish 或销毁必须保留仍在使用的候选，迟到任务只能结束借用。 */
static void pending_finish_and_ownership(void) {
    cases += 1U;
    Fixture f = fixture();
    OmniPixelOracle *oracle = create(&f);
    (void)observe(oracle, 1, f.old_bytes);
    (void)observe(oracle, 2, f.new_bytes);
    OmniPixelAuditBinding binding = {0};
    OmniPixelImage candidate = {0};
    require(omni_pixel_oracle_audit_begin(oracle, &binding, &candidate) == OMNI_PIXEL_OK, "begin pending close test");
    OmniPixelFooter before = omni_pixel_oracle_finish(oracle);
    require(!before.witness_valid && before.failure == OMNI_PIXEL_AUDIT_PENDING && before.owned_pixel_bytes_at_finish == 96,
            "premature footer invalid and outstanding bytes disclosed");
    require(omni_pixel_oracle_destroy(&oracle) == OMNI_PIXEL_AUDIT_PENDING && oracle != NULL,
            "cannot free while audit borrows candidate");
    require(candidate.bytes[4] == 0xaaU, "borrow remains alive after rejected destroy");
    OmniPixelRecord record = {0};
    require(omni_pixel_oracle_observe(oracle, frame(3), image(f.old_bytes), &record) == OMNI_PIXEL_SEALED,
            "late frame cannot revive sealed observer");
    finish_audit(oracle, binding, OMNI_PIXEL_SEALED);
    require(omni_pixel_oracle_owned_pixel_bytes(oracle) == 0, "late release frees outstanding actual allocation");
    OmniPixelFooter after = omni_pixel_oracle_finish(oracle);
    require(!after.witness_valid && after.failure == before.failure && after.recorded_frames == before.recorded_frames &&
            after.owned_pixel_bytes_at_finish == before.owned_pixel_bytes_at_finish &&
            after.audit_complete == before.audit_complete && after.audit_passed == before.audit_passed,
            "late successful audit cannot rewrite failed footer snapshot");
    destroy(&oracle);
}

/* 小上限、布局变化和未审计归还都是明确失败，不会退化成忽略帧。 */
static void additional_failure_edges(void) {
    cases += 1U;
    for (unsigned int kind = 0; kind < 4; ++kind) {
        /* 每种状态错误从独立两帧候选开始。 */
        Fixture f = fixture();
        f.config.frame_limit = 2;
        OmniPixelOracle *oracle = create(&f);
        (void)observe(oracle, 1, f.old_bytes);
        (void)observe(oracle, 2, f.new_bytes);
        OmniPixelAuditBinding binding = {0};
        OmniPixelImage candidate = {0};
        require(omni_pixel_oracle_audit_begin(oracle, &binding, &candidate) == OMNI_PIXEL_OK, "begin additional edge audit");
        if (kind == 0) {
            /* 实际工作方可以放弃结果，但归还本身不构成成功审计。 */
            require(omni_pixel_oracle_audit_release(oracle, binding) == OMNI_PIXEL_AUDIT_STATE, "release without result is abandonment");
        } else if (kind == 1) {
            /* 即便尚未写 footer，非法提前 free 也要保留失败。 */
            require(omni_pixel_oracle_destroy(&oracle) == OMNI_PIXEL_AUDIT_PENDING, "early destruction rejected");
            finish_audit(oracle, binding, OMNI_PIXEL_AUDIT_PENDING);
        } else if (kind == 2) {
            /* 第二个任务不取得另一个候选或清掉第一个借用。 */
            OmniPixelAuditBinding ignored_binding = {0};
            OmniPixelImage ignored_image = {0};
            require(omni_pixel_oracle_audit_begin(oracle, &ignored_binding, &ignored_image) == OMNI_PIXEL_AUDIT_STATE,
                    "only one audit job admitted");
            finish_audit(oracle, binding, OMNI_PIXEL_AUDIT_STATE);
        } else {
            /* 声明较小帧上限同样禁止覆盖已完成的记录。 */
            pass_audit(oracle, binding);
            OmniPixelRecord ignored = {0};
            require(omni_pixel_oracle_observe(oracle, frame(3), image(f.old_bytes), &ignored) == OMNI_PIXEL_FRAME_OVERFLOW,
                    "declared small frame cap enforced");
        }
        require(!omni_pixel_oracle_finish(oracle).witness_valid, "failed lifecycle never has valid witness");
        destroy(&oracle);
    }
    Fixture f = fixture();
    OmniPixelOracle *oracle = create(&f);
    (void)observe(oracle, 1, f.old_bytes);
    OmniPixelImage broken = image(f.new_bytes);
    broken.width = 7;
    OmniPixelRecord ignored = {0};
    require(omni_pixel_oracle_observe(oracle, frame(2), broken, &ignored) == OMNI_PIXEL_INVALID_GEOMETRY,
            "runtime geometry change fails, not UNKNOWN");
    require(!omni_pixel_oracle_finish(oracle).witness_valid, "geometry failure persists to footer");
    destroy(&oracle);
}

/* 临近 u64 上限的原始 ticks 不经浮点或窄整数，finish 之后的调用不改变已冻结记录。 */
static void wide_ticks_and_sealed_footer(void) {
    cases += 1U;
    Fixture f = fixture();
    f.config.start_ticks = UINT64_MAX - 100U;
    OmniPixelOracle *oracle = create(&f);
    OmniPixelFrame old = {17, 1, UINT64_MAX - 3U, UINT64_MAX - 2U};
    OmniPixelFrame new_frame = {17, 2, UINT64_MAX - 1U, UINT64_MAX};
    OmniPixelRecord record = {0};
    require(omni_pixel_oracle_observe(oracle, old, image(f.old_bytes), &record) == OMNI_PIXEL_OK,
            "full u64 OLD clock admitted exactly");
    require(omni_pixel_oracle_observe(oracle, new_frame, image(f.new_bytes), &record) == OMNI_PIXEL_OK,
            "full u64 NEW clock admitted exactly");
    OmniPixelAuditBinding binding = {0};
    OmniPixelImage candidate = {0};
    require(omni_pixel_oracle_audit_begin(oracle, &binding, &candidate) == OMNI_PIXEL_OK &&
            binding.display_ticks == UINT64_MAX - 1U, "audit preserves high-bit candidate ticks");
    pass_audit(oracle, binding);
    OmniPixelFooter footer = omni_pixel_oracle_finish(oracle);
    require(footer.witness_valid && footer.pre_candidate_frames_all_old && footer.timeline_complete_for_classification,
            "complete two-state classified witness");
    require(footer.earliest_observed.display_ticks == UINT64_MAX - 1U && !footer.latency_acceptance &&
            !footer.capture_completeness_verified, "wide scalar witness still makes no paint acceptance claim");
    require(omni_pixel_oracle_audit_begin(oracle, &binding, &candidate) == OMNI_PIXEL_SEALED,
            "cannot start another audit after valid finish");
    OmniPixelState state = OMNI_PIXEL_UNKNOWN;
    require(omni_pixel_oracle_classify(oracle, image(f.old_bytes), &state) == OMNI_PIXEL_SEALED,
            "cannot read freed templates after finish");
    require(omni_pixel_oracle_finish(oracle).earliest_observed.display_ticks == UINT64_MAX - 1U,
            "sealed footer cannot drift");
    destroy(&oracle);
}

/* 活动状态调用错误不可静默消失；UNKNOWN-only 或未取得 audit 结果也不能通过。 */
static void missing_audits_and_arguments(void) {
    cases += 1U;
    for (unsigned int kind = 0; kind < 5; ++kind) {
        /* 不共享已失败状态，使每个失败来源独立可达。 */
        Fixture f = fixture();
        OmniPixelOracle *oracle = create(&f);
        if (kind == 4) {
            /* 纯 UNKNOWN 没有提供 OLD 或 NEW 的正向证据。 */
            f.current[1U * 40U + 2U * 4U] ^= 1U;
            require(observe(oracle, 1, f.current).state == OMNI_PIXEL_UNKNOWN, "unknown-only sample recorded");
        } else {
            /* 其他分支从正常候选开始，不能把先前成功当作忽略后续调用错误的理由。 */
            (void)observe(oracle, 1, f.old_bytes);
            (void)observe(oracle, 2, f.new_bytes);
            OmniPixelAuditBinding binding = {0};
            OmniPixelImage candidate = {0};
            if (kind == 0) {
                /* 缺少记录输出槽属于真实时序 API 错误。 */
                require(omni_pixel_oracle_observe(oracle, frame(3), image(f.new_bytes), NULL) == OMNI_PIXEL_INVALID_ARGUMENT,
                        "missing active record output fails closed");
            } else if (kind == 1) {
                /* 缺少审计输出槽不能变成一次忽略的调用。 */
                require(omni_pixel_oracle_audit_begin(oracle, NULL, &candidate) == OMNI_PIXEL_INVALID_ARGUMENT,
                        "missing audit binding output fails closed");
            } else if (kind == 2) {
                /* 有候选而从未审计，finish 必须失败并释放没有借出的像素。 */
                require(omni_pixel_oracle_owned_pixel_bytes(oracle) != 0, "unaudited candidate still owned before finish");
            } else {
                /* 正常结束后重复归还仍是协议错误，不能伪造第二个成功。 */
                require(omni_pixel_oracle_audit_begin(oracle, &binding, &candidate) == OMNI_PIXEL_OK,
                        "begin duplicate release edge");
                pass_audit(oracle, binding);
                require(omni_pixel_oracle_audit_release(oracle, binding) == OMNI_PIXEL_AUDIT_STATE,
                        "duplicate release fails closed");
            }
        }
        OmniPixelFooter footer = omni_pixel_oracle_finish(oracle);
        require(!footer.witness_valid && footer.owned_pixel_bytes_at_finish == 0,
                "invalid final witness releases all nonborrowed capacity");
        destroy(&oracle);
    }
    require(omni_pixel_oracle_destroy(NULL) == OMNI_PIXEL_INVALID_ARGUMENT, "null owner slot rejected");
    OmniPixelOracle *empty = NULL;
    require(omni_pixel_oracle_destroy(&empty) == OMNI_PIXEL_OK, "empty owner slot cleanup idempotent");
}

/* 源行可以换成紧凑 stride；只复制 ROI 的活动像素，绝不把 padding 当成模板。 */
static void different_valid_strides(void) {
    cases += 1U;
    Fixture f = fixture();
    OmniPixelOracle *oracle = create(&f);
    uint8_t compact[128];
    for (size_t row = 0; row < 4; ++row) {
        /* 活动行从不同 padding 的参考源复制到独立紧凑内存。 */
        memcpy(compact + row * 32U, f.new_bytes + row * 40U, 32U);
    }
    OmniPixelImage packed = {compact, sizeof(compact), 8, 4, 32};
    OmniPixelState state = OMNI_PIXEL_UNKNOWN;
    require(omni_pixel_oracle_classify(oracle, packed, &state) == OMNI_PIXEL_OK && state == OMNI_PIXEL_NEW,
            "valid stride changes preserve exact active ROI matching");
    (void)observe(oracle, 1, f.old_bytes);
    OmniPixelRecord record = {0};
    require(omni_pixel_oracle_observe(oracle, frame(2), packed, &record) == OMNI_PIXEL_OK,
            "candidate may originate in compact input");
    OmniPixelAuditBinding binding = {0};
    OmniPixelImage candidate = {0};
    require(omni_pixel_oracle_audit_begin(oracle, &binding, &candidate) == OMNI_PIXEL_OK, "begin packed candidate audit");
    for (size_t row = 0; row < 2; ++row) {
        /* 候选紧凑行必须完整等于该时刻源 ROI，不能只检查一个识别字节。 */
        require(memcmp(candidate.bytes + row * 16U, compact + (row + 1U) * 32U + 8U, 16U) == 0,
                "all candidate ROI rows are exact independent copies");
    }
    memset(compact, 0, sizeof(compact));
    require(candidate.bytes[4] == 0xaaU, "source mutation cannot change same-frame candidate");
    pass_audit(oracle, binding);
    require(omni_pixel_oracle_finish(oracle).witness_valid, "packed witness completes without source ownership");
    destroy(&oracle);
}

/* 只有纯逻辑组全部成功才输出固定标量摘要，不调用任何捕获或权限接口。 */
int main(void) {
    exact_pixels();
    geometry_and_configuration();
    fixed_mask();
    total_pixel_budget();
    unknown_and_positive_baseline();
    delayed_audit_600_frames(false);
    delayed_audit_600_frames(true);
    ordering_failures();
    preexisting_or_missing();
    mismatched_audits();
    rejected_and_duplicate_audits();
    pending_finish_and_ownership();
    additional_failure_edges();
    wide_ticks_and_sealed_footer();
    missing_audits_and_arguments();
    different_valid_strides();
    printf("{\"pure_cases\":%u,\"pure_checks\":%u,\"max_replay_frames\":600,\"owned_scalar_bytes\":%zu,\"peak_owned_pixel_bytes\":%zu,\"capture_invocations\":0,\"native_audits\":0,\"latency_acceptance\":false}\n",
           cases, checks, observed_scalar_bytes, peak_owned_pixel_bytes);
    return EXIT_SUCCESS;
}
