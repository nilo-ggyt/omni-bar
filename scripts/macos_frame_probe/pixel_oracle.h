/* 纯 BGRA 模板与标量时序合同；不调用 SCK、Vision、时钟、文件或任何原生 UI。 */
#ifndef OMNI_FRAME_PIXEL_ORACLE_H
#define OMNI_FRAME_PIXEL_ORACLE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define OMNI_PIXEL_MAX_FRAMES 600U
#define OMNI_PIXEL_MAX_WIDTH 512U
#define OMNI_PIXEL_MAX_HEIGHT 128U
#define OMNI_PIXEL_MAX_OWNED_BYTES 262144U

/* 调用期间借用有效、稳定的 BGRA bytes；byte_count 必须描述调用方实际可读的整个范围。 */
typedef struct {
    const uint8_t *bytes;
    size_t byte_count;
    size_t width;
    size_t height;
    size_t bytes_per_row;
} OmniPixelImage;

/* ROI 坐标属于输入 image；mask 坐标属于该 ROI，全部采用整数像素且不作缩放或颜色修复。 */
typedef struct {
    size_t x;
    size_t y;
    size_t width;
    size_t height;
} OmniPixelRect;

/* mask 只能忽略 OLD/NEW 原字节本来就相同的固定矩形，不能遮住任何模板差异。
 * 同字节检查不证明区域里没有文字；接线方仍须核对完整字形/边界，候选还必须同帧审计。 */
typedef struct {
    bool enabled;
    OmniPixelRect rect;
} OmniPixelMask;

/* UNKNOWN 只表示与两个完整模板都不同，不能用作 NEW 不存在的证据。 */
typedef enum {
    OMNI_PIXEL_UNKNOWN,
    OMNI_PIXEL_OLD,
    OMNI_PIXEL_NEW
} OmniPixelState;

/* 错误均为有限标签；第一个时序或审计失败永久保留，不以迟到成功覆盖。 */
typedef enum {
    OMNI_PIXEL_OK,
    OMNI_PIXEL_INVALID_ARGUMENT,
    OMNI_PIXEL_INVALID_GEOMETRY,
    OMNI_PIXEL_BUDGET_EXCEEDED,
    OMNI_PIXEL_MASK_HIDES_DIFFERENCE,
    OMNI_PIXEL_IDENTICAL_TEMPLATES,
    OMNI_PIXEL_ALLOCATION_FAILED,
    OMNI_PIXEL_SEALED,
    OMNI_PIXEL_FRAME_ORDER,
    OMNI_PIXEL_PREEXISTING_NEW,
    OMNI_PIXEL_FRAME_OVERFLOW,
    OMNI_PIXEL_AUDIT_IDENTITY,
    OMNI_PIXEL_AUDIT_STATE,
    OMNI_PIXEL_AUDIT_REJECTED,
    OMNI_PIXEL_AUDIT_PENDING,
    OMNI_PIXEL_NO_WITNESS
} OmniPixelStatus;

/* run 与源几何在创建后不可变；时钟由外部声明，模块不把任意 tick 自动解释为 SCK 呈现。 */
typedef struct {
    uint64_t run_id;
    uint64_t start_ticks;
    uint32_t required_baseline_frames;
    uint32_t frame_limit;
    OmniPixelRect roi;
    OmniPixelMask mask;
} OmniPixelConfig;

/* 连续 index 只证明传入本模块的序列，没有声称它是系统完整 display-frame 编号。 */
typedef struct {
    uint64_t run_id;
    uint32_t index;
    uint64_t display_ticks;
    uint64_t callback_absolute_ticks;
} OmniPixelFrame;

/* 每个收到的有效帧只保留标量；pending audit 不阻止后续帧分类，也不移动首个候选。 */
typedef struct {
    OmniPixelFrame frame;
    OmniPixelState state;
    bool baseline_ready;
    bool earliest_observed_candidate;
} OmniPixelRecord;

/* 唯一候选的同帧身份；外部审计必须原样回传三项，不允许用较晚图像回填较早 ticks。 */
typedef struct {
    uint64_t run_id;
    uint32_t frame_index;
    uint64_t display_ticks;
} OmniPixelAuditBinding;

/* footer 只证明一个已审计的较晚 NEW witness；不能授权输入因果、首次呈现或延迟通过。
 * timeline_complete_for_classification 仅指已传入帧都可明确分类，不证明 SCK 实际交付完整。 */
typedef struct {
    bool witness_valid;
    bool pre_candidate_frames_all_old;
    bool timeline_complete_for_classification;
    bool capture_completeness_verified;
    bool latency_acceptance;
    OmniPixelStatus failure;
    uint32_t recorded_frames;
    uint32_t unknown_frames;
    uint32_t unknown_before_candidate;
    bool baseline_ready;
    bool candidate_present;
    OmniPixelAuditBinding earliest_observed;
    bool audit_complete;
    bool audit_passed;
    bool audit_loan_released;
    size_t peak_owned_pixel_bytes;
    size_t owned_pixel_bytes_at_finish;
    size_t owned_scalar_bytes;
} OmniPixelFooter;

/* 单线程顺序 owner；调用方须串行化，模块不创建工作线程、全局缓存或异步回调。 */
typedef struct OmniPixelOracle OmniPixelOracle;

/* 复制两份 ROI 并预留一份候选，三份实际分配总量不得超过 256 KiB；失败不修改输入。 */
OmniPixelOracle *omni_pixel_oracle_create(OmniPixelConfig config,
                                        OmniPixelImage old_image,
                                        OmniPixelImage new_image,
                                        OmniPixelStatus *status);

/* 仅分类，不推进时序；源尺寸必须与创建时相同，stride 可变化但不能缺失任何活动像素。 */
OmniPixelStatus omni_pixel_oracle_classify(const OmniPixelOracle *oracle,
                                         OmniPixelImage image,
                                         OmniPixelState *state);

/* 错误不会计为成功记录；UNKNOWN 中断连续 OLD baseline，但不冒充 absence 或立即销毁 witness。 */
OmniPixelStatus omni_pixel_oracle_observe(OmniPixelOracle *oracle,
                                        OmniPixelFrame frame,
                                        OmniPixelImage image,
                                        OmniPixelRecord *record);

/* 借出唯一候选的只读像素；直到 audit_release 前 owner 不得销毁，候选 bytes 永不复写。 */
OmniPixelStatus omni_pixel_oracle_audit_begin(OmniPixelOracle *oracle,
                                            OmniPixelAuditBinding *binding,
                                            OmniPixelImage *candidate);

/* 外部回执必须来自该候选；两个 bool 是调用方声明，本纯模块不验证真实 OCR 或 native release。
 * sealed 后直接返回 SEALED 且不记录结果；已有首错时只记录清理事实并返回首错，不恢复观测。 */
OmniPixelStatus omni_pixel_oracle_audit_complete(OmniPixelOracle *oracle,
                                               OmniPixelAuditBinding binding,
                                               bool expected_validated,
                                               bool frame_graph_released);

/* 审计方结束所有像素借用后显式归还；失败后也可归还以清理，但永远不能恢复有效 witness。
 * 归还前不得仍有持此 bytes 的外部 image/handler；模块无法用 bool 独立检查该调用方义务。 */
OmniPixelStatus omni_pixel_oracle_audit_release(OmniPixelOracle *oracle,
                                              OmniPixelAuditBinding binding);

/* 第一次调用永久封闭采集并冻结 footer；未完成/未归还的 audit 产生失败，迟到回执不能改写它。 */
OmniPixelFooter omni_pixel_oracle_finish(OmniPixelOracle *oracle);

/* 只复制有限标量，不返回内部记录借用；out-of-range 不改变输出。 */
bool omni_pixel_oracle_record(const OmniPixelOracle *oracle, uint32_t offset, OmniPixelRecord *record);

/* 有效 owner 的实际分配计数；scalar 包括固定 600 条记录及 owner 元数据，不含任何框架内存。 */
size_t omni_pixel_oracle_owned_pixel_bytes(const OmniPixelOracle *oracle);
size_t omni_pixel_oracle_owned_scalar_bytes(const OmniPixelOracle *oracle);

/* 未归还审计借用时拒绝 free 并保留指针；其余路径释放实际像素/标量 owner 并置空调用方槽。 */
OmniPixelStatus omni_pixel_oracle_destroy(OmniPixelOracle **oracle);

#endif
