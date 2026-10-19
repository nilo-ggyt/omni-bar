/* 纯像素匹配与有界候选时序；真实采集、语义审计和时钟来源必须由独立适配器证明。 */
#include "pixel_oracle.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* 唯一堆 owner 同时拥有三份像素和固定记录；不保存任何调用方源指针。 */
struct OmniPixelOracle {
    OmniPixelConfig config;
    size_t source_width;
    size_t source_height;
    size_t template_bytes;
    size_t owned_pixel_bytes;
    size_t peak_pixel_bytes;
    uint8_t *pixels;
    OmniPixelRecord records[OMNI_PIXEL_MAX_FRAMES];
    uint32_t count;
    uint32_t unknown_count;
    uint32_t unknown_before_candidate;
    uint32_t baseline_count;
    bool baseline_ready;
    bool candidate_present;
    OmniPixelAuditBinding candidate;
    bool audit_started;
    bool audit_complete;
    bool audit_passed;
    bool audit_loan;
    bool sealed;
    OmniPixelStatus failure;
    OmniPixelFooter footer;
};

/* 首错不可被后续成功消除；返回当前错误，同时保留最终 footer 所需的原始失败原因。 */
static OmniPixelStatus fail(OmniPixelOracle *oracle, OmniPixelStatus status) {
    if (oracle->failure == OMNI_PIXEL_OK) {
        /* 只有首次失败能取得最终失败身份，迟到审计不能改写它。 */
        oracle->failure = status;
    }
    return status;
}

/* 先证明活动行和总可读范围；padding 不属于模板，但行跨度也不能溢出。 */
static bool image_valid(OmniPixelImage image) {
    if (image.bytes == NULL || image.width == 0 || image.height == 0 || image.width > SIZE_MAX / 4U) {
        /* 没有完整 BGRA 活动行的输入不能进入指针计算。 */
        return false;
    }
    size_t active = image.width * 4U;
    if (image.bytes_per_row < active || image.height - 1U > (SIZE_MAX - active) / image.bytes_per_row) {
        /* 拒绝短 stride 或累计行偏移溢出，不把缺失像素解释成空白。 */
        return false;
    }
    size_t required = (image.height - 1U) * image.bytes_per_row + active;
    return required <= image.byte_count && required <= (size_t)PTRDIFF_MAX;
}

/* 减法界限避免 x+width 溢出；空或越界矩形都不参与部分匹配。 */
static bool rect_inside(OmniPixelRect rect, size_t width, size_t height) {
    return rect.width != 0 && rect.height != 0 && rect.x <= width && rect.y <= height &&
           rect.width <= width - rect.x && rect.height <= height - rect.y;
}

/* mask 只拥有一个已验证的有限矩形；禁用时不能暗藏另一份非零配置。 */
static bool mask_valid(OmniPixelMask mask, size_t width, size_t height) {
    if (mask.enabled) {
        /* 开启的遮罩必须完全包含在 ROI 内，不能裁切成另一个未知区域。 */
        return rect_inside(mask.rect, width, height);
    }
    return mask.rect.x == 0 && mask.rect.y == 0 && mask.rect.width == 0 && mask.rect.height == 0;
}

/* x/y 已在有限 ROI 迭代中；矩形加法由创建时的包含关系证明不会溢出。 */
static bool masked(OmniPixelMask mask, size_t x, size_t y) {
    return mask.enabled && x >= mask.rect.x && x < mask.rect.x + mask.rect.width &&
           y >= mask.rect.y && y < mask.rect.y + mask.rect.height;
}

/* 只复制配置 ROI 的活动像素；不保留父图像、padding 或源 provider。 */
static void copy_roi(uint8_t *destination, OmniPixelImage image, OmniPixelRect roi) {
    size_t row_bytes = roi.width * 4U;
    for (size_t row = 0; row < roi.height; ++row) {
        /* image/ROI 验证保证读写都位于实际声明范围内，循环次数受 128 行上限约束。 */
        memcpy(destination + row * row_bytes,
               image.bytes + (roi.y + row) * image.bytes_per_row + roi.x * 4U,
               row_bytes);
    }
}

/* 所有失败构造都只释放本次 owner；调用方图像始终只是借用。 */
OmniPixelOracle *omni_pixel_oracle_create(OmniPixelConfig config,
                                        OmniPixelImage old_image,
                                        OmniPixelImage new_image,
                                        OmniPixelStatus *status) {
    OmniPixelStatus error = OMNI_PIXEL_OK;
    if (config.run_id == 0 || config.start_ticks == 0 || config.required_baseline_frames == 0 ||
        config.required_baseline_frames > 10U || config.frame_limit == 0 || config.frame_limit > OMNI_PIXEL_MAX_FRAMES) {
        /* 不允许零身份、无起点或无界状态；这些值无法支撑后续唯一时序。 */
        error = OMNI_PIXEL_INVALID_ARGUMENT;
    } else if (!image_valid(old_image) || !image_valid(new_image) || old_image.width != new_image.width ||
               old_image.height != new_image.height || !rect_inside(config.roi, old_image.width, old_image.height) ||
               config.roi.width > OMNI_PIXEL_MAX_WIDTH || config.roi.height > OMNI_PIXEL_MAX_HEIGHT ||
               !mask_valid(config.mask, config.roi.width, config.roi.height)) {
        /* 源几何和固定 mask 必须同时成立，不能从两张不同布局中学习一个猜测模板。 */
        error = OMNI_PIXEL_INVALID_GEOMETRY;
    } else if (config.roi.width * config.roi.height * 4U > OMNI_PIXEL_MAX_OWNED_BYTES / 3U) {
        /* 模板与候选三份都计入实际像素预算，不只报告单个 crop 的大小。 */
        error = OMNI_PIXEL_BUDGET_EXCEEDED;
    }
    if (error != OMNI_PIXEL_OK) {
        /* 输入拒绝不分配 owner，也不修改调用方图像。 */
        if (status != NULL) {
            /* 错误输出可选，但存在时必须得到精确拒绝原因。 */
            *status = error;
        }
        return NULL;
    }
    OmniPixelOracle *oracle = calloc(1U, sizeof(*oracle));
    size_t bytes = config.roi.width * config.roi.height * 4U;
    if (oracle != NULL) {
        /* 三份存储一次分配，候选空间已经预留，测量过程中不再扩容像素或记录。 */
        oracle->pixels = malloc(bytes * 3U);
    }
    if (oracle == NULL || oracle->pixels == NULL) {
        /* 分配只成功一半也不能遗留 owner；没有任何源像素 owner 被转移。 */
        free(oracle);
        if (status != NULL) {
            /* 内存耗尽是工具失败，不能降级为空模板或部分对比。 */
            *status = OMNI_PIXEL_ALLOCATION_FAILED;
        }
        return NULL;
    }
    oracle->config = config;
    oracle->source_width = old_image.width;
    oracle->source_height = old_image.height;
    oracle->template_bytes = bytes;
    oracle->owned_pixel_bytes = bytes * 3U;
    oracle->peak_pixel_bytes = bytes * 3U;
    copy_roi(oracle->pixels, old_image, config.roi);
    copy_roi(oracle->pixels + bytes, new_image, config.roi);
    bool different = false;
    for (size_t y = 0; y < config.roi.height; ++y) {
        /* 每个 mask 像素都按原始四字节核验，不能用阈值把差异隐藏掉。 */
        for (size_t x = 0; x < config.roi.width; ++x) {
            size_t offset = (y * config.roi.width + x) * 4U;
            bool equal = memcmp(oracle->pixels + offset, oracle->pixels + bytes + offset, 4U) == 0;
            if (masked(config.mask, x, y) && !equal) {
                /* 任一模板差异落入遮罩就拒绝整个配置，不自动缩小或移动 mask。 */
                error = OMNI_PIXEL_MASK_HIDES_DIFFERENCE;
            }
            different |= !equal;
        }
    }
    if (!different && error == OMNI_PIXEL_OK) {
        /* 相同模板不能区分 OLD/NEW，也不能支撑正向 baseline。 */
        error = OMNI_PIXEL_IDENTICAL_TEMPLATES;
    }
    if (error != OMNI_PIXEL_OK) {
        /* 未发布模板失败时释放三份分配，绝不留下可被调用的半成品。 */
        free(oracle->pixels);
        free(oracle);
        oracle = NULL;
    }
    if (status != NULL) {
        /* 成功与失败都明确写出结果，不依赖调用方原有错误值。 */
        *status = error;
    }
    return oracle;
}

/* 比较只忽略固定同字节区域，不做 hash-only、颜色容差、前缀或平移匹配。 */
OmniPixelStatus omni_pixel_oracle_classify(const OmniPixelOracle *oracle,
                                         OmniPixelImage image,
                                         OmniPixelState *state) {
    if (oracle == NULL || state == NULL) {
        /* 缺失 owner 或结果槽不产生一个伪 UNKNOWN 成功。 */
        return OMNI_PIXEL_INVALID_ARGUMENT;
    }
    if (oracle->sealed) {
        /* finish 可以已释放模板，迟到分类不能访问原像素。 */
        return OMNI_PIXEL_SEALED;
    }
    if (!image_valid(image) || image.width != oracle->source_width || image.height != oracle->source_height) {
        /* 几何变化是错误；不能把换窗、缩放或截断伪装成内容 UNKNOWN。 */
        return OMNI_PIXEL_INVALID_GEOMETRY;
    }
    bool old_equal = true;
    bool new_equal = true;
    OmniPixelRect roi = oracle->config.roi;
    for (size_t y = 0; y < roi.height; ++y) {
        /* 每轮只读取当前已证明活动行；调用期间源像素必须稳定。 */
        const uint8_t *source = image.bytes + (roi.y + y) * image.bytes_per_row + roi.x * 4U;
        for (size_t x = 0; x < roi.width; ++x) {
            if (masked(oracle->config.mask, x, y)) {
                /* 模板在此必须相同；这只放宽固定像素比较，不能独立证明遮罩中没有文字。 */
                continue;
            }
            size_t offset = (y * roi.width + x) * 4U;
            old_equal &= memcmp(source + x * 4U, oracle->pixels + offset, 4U) == 0;
            new_equal &= memcmp(source + x * 4U, oracle->pixels + oracle->template_bytes + offset, 4U) == 0;
        }
    }
    *state = old_equal ? OMNI_PIXEL_OLD : (new_equal ? OMNI_PIXEL_NEW : OMNI_PIXEL_UNKNOWN);
    return OMNI_PIXEL_OK;
}

/* 身份等值比较不用结构体 memcmp，避免 padding 或字节布局进入协议判断。 */
static bool binding_equal(OmniPixelAuditBinding a, OmniPixelAuditBinding b) {
    return a.run_id == b.run_id && a.frame_index == b.frame_index && a.display_ticks == b.display_ticks;
}

/* 每帧推进都有限且不等待审计；任何失败都禁止以后签发有效 witness。 */
OmniPixelStatus omni_pixel_oracle_observe(OmniPixelOracle *oracle,
                                        OmniPixelFrame frame,
                                        OmniPixelImage image,
                                        OmniPixelRecord *record) {
    if (oracle == NULL) {
        /* 没有 owner 的调用不参与时序。 */
        return OMNI_PIXEL_INVALID_ARGUMENT;
    }
    if (oracle->sealed) {
        /* 最终 footer 已冻结，迟到 callback 不修改它或访问已释放模板。 */
        return OMNI_PIXEL_SEALED;
    }
    if (record == NULL) {
        /* 活动观测缺少记录槽属于调用错误，不能忽略这次失败后再签发有效 witness。 */
        return fail(oracle, OMNI_PIXEL_INVALID_ARGUMENT);
    }
    if (oracle->failure != OMNI_PIXEL_OK) {
        /* 首错保持失败，不靠后续正常帧恢复测量。 */
        return oracle->failure;
    }
    if (oracle->count == oracle->config.frame_limit) {
        /* 第 601 帧或更小声明上限之外的帧不能覆盖旧记录。 */
        return fail(oracle, OMNI_PIXEL_FRAME_OVERFLOW);
    }
    uint64_t last_display = oracle->count == 0 ? oracle->config.start_ticks : oracle->records[oracle->count - 1U].frame.display_ticks;
    uint64_t last_callback = oracle->count == 0 ? oracle->config.start_ticks : oracle->records[oracle->count - 1U].frame.callback_absolute_ticks;
    if (frame.run_id != oracle->config.run_id || frame.index != oracle->count + 1U ||
        frame.display_ticks <= last_display || frame.callback_absolute_ticks <= last_callback ||
        frame.callback_absolute_ticks < frame.display_ticks) {
        /* 错 run、跳号、重复或倒退时间不能进入同一次观测，也不能替换最早候选。 */
        return fail(oracle, OMNI_PIXEL_FRAME_ORDER);
    }
    OmniPixelState state = OMNI_PIXEL_UNKNOWN;
    OmniPixelStatus classified = omni_pixel_oracle_classify(oracle, image, &state);
    if (classified != OMNI_PIXEL_OK) {
        /* 格式/布局错误与 UNKNOWN 不同，整体失败且不增加成功帧数。 */
        return fail(oracle, classified);
    }
    if (!oracle->baseline_ready && state == OMNI_PIXEL_NEW) {
        /* 正向 OLD baseline 之前出现 NEW 属于预先存在，不能算一次快速输入结果。 */
        return fail(oracle, OMNI_PIXEL_PREEXISTING_NEW);
    }
    if (state == OMNI_PIXEL_UNKNOWN) {
        /* UNKNOWN 单独计数并打断连续 OLD；候选前的不确定性永远不会被抹去。 */
        oracle->unknown_count += 1U;
        if (!oracle->candidate_present) {
            /* 之后的已审计 NEW 仍可成为上界 witness，但不能证明此前始终是 OLD。 */
            oracle->unknown_before_candidate += 1U;
        }
    }
    if (!oracle->baseline_ready) {
        /* 在尚未建立 baseline 时，只认可连续、精确的 OLD 图像。 */
        oracle->baseline_count = state == OMNI_PIXEL_OLD ? oracle->baseline_count + 1U : 0U;
        oracle->baseline_ready = oracle->baseline_count >= oracle->config.required_baseline_frames;
    }
    bool candidate = oracle->baseline_ready && state == OMNI_PIXEL_NEW && !oracle->candidate_present;
    if (candidate) {
        /* 候选像素与身份同在本次同步调用复制，后续分类和慢审计都不能覆盖它。 */
        copy_roi(oracle->pixels + oracle->template_bytes * 2U, image, oracle->config.roi);
        oracle->candidate = (OmniPixelAuditBinding){frame.run_id, frame.index, frame.display_ticks};
        oracle->candidate_present = true;
    }
    OmniPixelRecord value = {frame, state, oracle->baseline_ready, candidate};
    oracle->records[oracle->count++] = value;
    *record = value;
    return OMNI_PIXEL_OK;
}

/* 只借出同一个已复制候选；调用方须让此 owner 活到审计像素借用归还。 */
OmniPixelStatus omni_pixel_oracle_audit_begin(OmniPixelOracle *oracle,
                                            OmniPixelAuditBinding *binding,
                                            OmniPixelImage *candidate) {
    if (oracle == NULL) {
        /* 无 owner 时不创建借用。 */
        return OMNI_PIXEL_INVALID_ARGUMENT;
    }
    if (oracle->sealed) {
        /* 已结束测量不再开启晚到审计，也不复活被释放的模板。 */
        return OMNI_PIXEL_SEALED;
    }
    if (binding == NULL || candidate == NULL) {
        /* 活动 owner 的缺失输出槽不能退化成一次可忽略的审计尝试。 */
        return fail(oracle, OMNI_PIXEL_INVALID_ARGUMENT);
    }
    if (oracle->failure != OMNI_PIXEL_OK) {
        /* 失败观测不再创建新的工作载荷。 */
        return oracle->failure;
    }
    if (!oracle->candidate_present || oracle->audit_started) {
        /* 没有候选或第二次请求都不能构造另一个任务来替换失败/迟到的审计。 */
        return fail(oracle, OMNI_PIXEL_AUDIT_STATE);
    }
    oracle->audit_started = true;
    oracle->audit_loan = true;
    *binding = oracle->candidate;
    *candidate = (OmniPixelImage){oracle->pixels + oracle->template_bytes * 2U,
                                  oracle->template_bytes, oracle->config.roi.width,
                                  oracle->config.roi.height, oracle->config.roi.width * 4U};
    return OMNI_PIXEL_OK;
}

/* 审计结果仅消费外部声明，不能用 bool 伪装本模块已经执行过 Vision 或 native weak 检查。 */
OmniPixelStatus omni_pixel_oracle_audit_complete(OmniPixelOracle *oracle,
                                               OmniPixelAuditBinding binding,
                                               bool expected_validated,
                                               bool frame_graph_released) {
    if (oracle == NULL) {
        /* 无 owner 的回执无法归属任何测量。 */
        return OMNI_PIXEL_INVALID_ARGUMENT;
    }
    if (oracle->sealed) {
        /* 最终结果已冻结；迟到回执不再修改内部审计结果，像素借用只能另行归还清理。 */
        return OMNI_PIXEL_SEALED;
    }
    if (!binding_equal(binding, oracle->candidate)) {
        /* 错身份不能归还真正候选的权限；保留真实借用供原审计清理。 */
        return fail(oracle, OMNI_PIXEL_AUDIT_IDENTITY);
    }
    if (!oracle->audit_started || oracle->audit_complete || !oracle->audit_loan) {
        /* 同一次审计只登记一次；重复或未取得借用的回执都保持失败。 */
        return fail(oracle, OMNI_PIXEL_AUDIT_STATE);
    }
    oracle->audit_complete = true;
    oracle->audit_passed = expected_validated && frame_graph_released;
    if (!oracle->audit_passed) {
        /* 内容或释放任一未被外部证明，就不能签发 witness；借用仍须另行归还。 */
        (void)fail(oracle, OMNI_PIXEL_AUDIT_REJECTED);
    }
    // 任务可以在先前观测失败后正常返回；保留清理事实，但绝不以 OK 表示测量恢复。
    return oracle->failure;
}

/* 仅释放模块自己的像素分配；此函数只能在不再存在候选借用时运行。 */
static void release_pixels(OmniPixelOracle *oracle) {
    free(oracle->pixels);
    oracle->pixels = NULL;
    oracle->owned_pixel_bytes = 0;
}

/* 完成回执不等于像素借用结束；把二者分开，防止异步审计仍读图时 free。 */
OmniPixelStatus omni_pixel_oracle_audit_release(OmniPixelOracle *oracle,
                                              OmniPixelAuditBinding binding) {
    if (oracle == NULL) {
        /* 无 owner 时不存在可归还租约。 */
        return OMNI_PIXEL_INVALID_ARGUMENT;
    }
    if (!binding_equal(binding, oracle->candidate)) {
        /* 错 run/index/ticks 的释放不能提前归还真正候选。 */
        return fail(oracle, OMNI_PIXEL_AUDIT_IDENTITY);
    }
    if (!oracle->audit_loan) {
        /* 重复归还不改变 owner，但会阻止未结束观测得到成功结果。 */
        return fail(oracle, OMNI_PIXEL_AUDIT_STATE);
    }
    oracle->audit_loan = false;
    OmniPixelStatus result = OMNI_PIXEL_OK;
    if (!oracle->audit_complete && !oracle->sealed) {
        /* 可显式放弃工作以清理，但没有结果的归还不能成为审计通过。 */
        result = fail(oracle, OMNI_PIXEL_AUDIT_STATE);
    }
    if (oracle->sealed) {
        /* 提前 finish 已固定为无效；迟到归还只完成所有权清理，不改写旧 footer。 */
        release_pixels(oracle);
    }
    return result;
}

/* 冻结的是观测上界 witness，而不是输入、首次呈现、完整采集或物理 scanout 证明。 */
OmniPixelFooter omni_pixel_oracle_finish(OmniPixelOracle *oracle) {
    if (oracle == NULL) {
        /* 没有成功创建 owner 也只能得到明确无效结果。 */
        return (OmniPixelFooter){.failure = OMNI_PIXEL_INVALID_ARGUMENT};
    }
    if (oracle->sealed) {
        /* 迟到事件和审计只可清理资源，不能让先前无效的最终记录变成有效。 */
        return oracle->footer;
    }
    oracle->sealed = true;
    if (!oracle->candidate_present) {
        /* OLD 或 UNKNOWN 图像本身都不是 NEW witness。 */
        (void)fail(oracle, OMNI_PIXEL_NO_WITNESS);
    } else if (!oracle->audit_complete || oracle->audit_loan) {
        /* 必须先完成审计并结束全部像素借用，不能在任务还活动时写有效 footer。 */
        (void)fail(oracle, OMNI_PIXEL_AUDIT_PENDING);
    }
    if (!oracle->audit_loan) {
        /* 不再有外部像素借用时，结束模板和候选的全部真实容量，标量记录仍可导出。 */
        release_pixels(oracle);
    }
    oracle->footer = (OmniPixelFooter){
        .witness_valid = oracle->failure == OMNI_PIXEL_OK && oracle->candidate_present && oracle->audit_passed,
        .pre_candidate_frames_all_old = oracle->candidate_present && oracle->unknown_before_candidate == 0,
        .timeline_complete_for_classification = oracle->count != 0 && oracle->unknown_count == 0 && oracle->failure == OMNI_PIXEL_OK,
        .capture_completeness_verified = false,
        .latency_acceptance = false,
        .failure = oracle->failure,
        .recorded_frames = oracle->count,
        .unknown_frames = oracle->unknown_count,
        .unknown_before_candidate = oracle->unknown_before_candidate,
        .baseline_ready = oracle->baseline_ready,
        .candidate_present = oracle->candidate_present,
        .earliest_observed = oracle->candidate,
        .audit_complete = oracle->audit_complete,
        .audit_passed = oracle->audit_passed,
        .audit_loan_released = oracle->audit_started && !oracle->audit_loan,
        .peak_owned_pixel_bytes = oracle->peak_pixel_bytes,
        .owned_pixel_bytes_at_finish = oracle->owned_pixel_bytes,
        .owned_scalar_bytes = sizeof(*oracle),
    };
    return oracle->footer;
}

/* 导出一条标量副本，不泄露内部数组或任何图像 owner。 */
bool omni_pixel_oracle_record(const OmniPixelOracle *oracle, uint32_t offset, OmniPixelRecord *record) {
    if (oracle == NULL || record == NULL || offset >= oracle->count) {
        /* 错范围不会写输出，也不把容量未使用部分伪装成记录。 */
        return false;
    }
    *record = oracle->records[offset];
    return true;
}

/* 只报告本模块实际分配的像素，不估计系统或未来 Vision 的内部缓存。 */
size_t omni_pixel_oracle_owned_pixel_bytes(const OmniPixelOracle *oracle) {
    return oracle == NULL ? 0 : oracle->owned_pixel_bytes;
}

/* scalar 计数包含 owner 布局和 600 条固定记录，没有另一个可增长列表。 */
size_t omni_pixel_oracle_owned_scalar_bytes(const OmniPixelOracle *oracle) {
    return oracle == NULL ? 0 : sizeof(*oracle);
}

/* 不在析构中做审计或 IO；未归还借用时保留真实 owner，调用方必须先结束该工作。 */
OmniPixelStatus omni_pixel_oracle_destroy(OmniPixelOracle **oracle) {
    if (oracle == NULL) {
        /* 没有调用方 owner 槽无法安全转移释放责任。 */
        return OMNI_PIXEL_INVALID_ARGUMENT;
    }
    if (*oracle == NULL) {
        /* 空槽的幂等清理不创建状态。 */
        return OMNI_PIXEL_OK;
    }
    if ((*oracle)->audit_loan) {
        /* 此拒绝也保留失败，随后正常归还只允许清理，不让早期非法释放恢复为通过。 */
        return fail(*oracle, OMNI_PIXEL_AUDIT_PENDING);
    }
    release_pixels(*oracle);
    free(*oracle);
    *oracle = NULL;
    return OMNI_PIXEL_OK;
}
