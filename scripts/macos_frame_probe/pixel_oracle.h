/* Pure BGRA-template and scalar-sequence contract, without SCK, Vision, clock, filesystem, or native UI calls. */
#ifndef OMNI_FRAME_PIXEL_ORACLE_H
#define OMNI_FRAME_PIXEL_ORACLE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define OMNI_PIXEL_MAX_FRAMES 600U
#define OMNI_PIXEL_MAX_WIDTH 512U
#define OMNI_PIXEL_MAX_HEIGHT 128U
#define OMNI_PIXEL_MAX_OWNED_BYTES 262144U

/* Borrow valid, stable BGRA bytes for the call; byte_count must describe the caller's actually readable extent. */
typedef struct {
    const uint8_t *bytes;
    size_t byte_count;
    size_t width;
    size_t height;
    size_t bytes_per_row;
} OmniPixelImage;

/* ROI coordinates address the source image; mask coordinates address that ROI, in integer pixels without rescaling. */
typedef struct {
    size_t x;
    size_t y;
    size_t width;
    size_t height;
} OmniPixelRect;

/* A fixed mask may ignore only pixels whose original OLD/NEW bytes agree, never a template difference.
 * Equal bytes do not prove absence of text; the adapter must establish complete glyph bounds and audit the same candidate frame. */
typedef struct {
    bool enabled;
    OmniPixelRect rect;
} OmniPixelMask;

/* UNKNOWN means neither complete template matches; it is not evidence that NEW is absent. */
typedef enum {
    OMNI_PIXEL_UNKNOWN,
    OMNI_PIXEL_OLD,
    OMNI_PIXEL_NEW
} OmniPixelState;

/* Errors are finite labels; the first sequencing/audit failure survives every later successful callback. */
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

/* Run and source geometry are immutable after creation; caller-declared ticks do not independently prove SCK presentation. */
typedef struct {
    uint64_t run_id;
    uint64_t start_ticks;
    uint32_t required_baseline_frames;
    uint32_t frame_limit;
    OmniPixelRect roi;
    OmniPixelMask mask;
} OmniPixelConfig;

/* Consecutive indices establish only the supplied sequence, not a complete system display-frame sequence. */
typedef struct {
    uint64_t run_id;
    uint32_t index;
    uint64_t display_ticks;
    uint64_t callback_absolute_ticks;
} OmniPixelFrame;

/* Retain scalars for each accepted frame; pending audit neither blocks later classification nor moves the first candidate. */
typedef struct {
    OmniPixelFrame frame;
    OmniPixelState state;
    bool baseline_ready;
    bool earliest_observed_candidate;
} OmniPixelRecord;

/* Bind audit to the one candidate's exact run/index/ticks; a later image cannot certify an earlier timestamp. */
typedef struct {
    uint64_t run_id;
    uint32_t frame_index;
    uint64_t display_ticks;
} OmniPixelAuditBinding;

/* A footer establishes an audited later NEW witness only, without proving input causality, first presentation, or latency.
 * timeline_complete_for_classification concerns supplied frames only, never completeness of actual SCK delivery. */
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

/* A serial, single-threaded owner; callers serialize access, and no worker, global cache, or asynchronous callback is created. */
typedef struct OmniPixelOracle OmniPixelOracle;

/* Copy two ROIs and reserve one candidate, totaling at most 256 KiB of actual pixels; rejection never modifies inputs. */
OmniPixelOracle *omni_pixel_oracle_create(OmniPixelConfig config,
                                        OmniPixelImage old_image,
                                        OmniPixelImage new_image,
                                        OmniPixelStatus *status);

/* Classify without advancing sequence; dimensions stay fixed, while stride may vary if all active pixels remain readable. */
OmniPixelStatus omni_pixel_oracle_classify(const OmniPixelOracle *oracle,
                                         OmniPixelImage image,
                                         OmniPixelState *state);

/* Errors do not count as accepted records; UNKNOWN interrupts consecutive OLD baseline without proving absence or destroying a witness. */
OmniPixelStatus omni_pixel_oracle_observe(OmniPixelOracle *oracle,
                                        OmniPixelFrame frame,
                                        OmniPixelImage image,
                                        OmniPixelRecord *record);

/* Lend the sole candidate's immutable pixels; the owner cannot be destroyed until audit_release returns that loan. */
OmniPixelStatus omni_pixel_oracle_audit_begin(OmniPixelOracle *oracle,
                                            OmniPixelAuditBinding *binding,
                                            OmniPixelImage *candidate);

/* External receipts must identify this candidate; caller-supplied booleans cannot independently prove OCR or native release.
 * Sealed calls return SEALED without mutation; after prior failure, cleanup facts may update but cannot restore validity. */
OmniPixelStatus omni_pixel_oracle_audit_complete(OmniPixelOracle *oracle,
                                               OmniPixelAuditBinding binding,
                                               bool expected_validated,
                                               bool frame_graph_released);

/* Return the loan only after all audit pixel borrows end; failure still allows cleanup but never restores witness validity.
 * No external image/handler may retain these bytes afterward; this pure module cannot independently verify that caller obligation. */
OmniPixelStatus omni_pixel_oracle_audit_release(OmniPixelOracle *oracle,
                                              OmniPixelAuditBinding binding);

/* First finish seals collection and freezes the footer; incomplete or outstanding audit fails permanently despite late receipts. */
OmniPixelFooter omni_pixel_oracle_finish(OmniPixelOracle *oracle);

/* Copy bounded scalars without lending an internal record; out-of-range calls leave output unchanged. */
bool omni_pixel_oracle_record(const OmniPixelOracle *oracle, uint32_t offset, OmniPixelRecord *record);

/* Count this owner's actual allocation; scalar storage includes 600 fixed records and metadata, excluding framework memory. */
size_t omni_pixel_oracle_owned_pixel_bytes(const OmniPixelOracle *oracle);
size_t omni_pixel_oracle_owned_scalar_bytes(const OmniPixelOracle *oracle);

/* An outstanding audit loan refuses free and preserves the pointer; other paths free owned storage and clear the caller's slot. */
OmniPixelStatus omni_pixel_oracle_destroy(OmniPixelOracle **oracle);

#endif
