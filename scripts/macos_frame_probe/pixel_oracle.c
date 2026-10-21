/* Pure pixel matching and bounded candidate sequencing; a real adapter must establish capture, semantic audit, and clock provenance. */
#include "pixel_oracle.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* One heap owner holds three pixel buffers and fixed records, retaining no caller source pointer. */
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

/* Preserve the first failure for the footer while returning the current error; later success cannot erase it. */
static OmniPixelStatus fail(OmniPixelOracle *oracle, OmniPixelStatus status) {
    if (oracle->failure == OMNI_PIXEL_OK) {
        /* Only the first failure acquires final failure identity; a late audit cannot replace it. */
        oracle->failure = status;
    }
    return status;
}

/* Establish active-row width and total readable extent first; padding is excluded, but stride arithmetic must also fit. */
static bool image_valid(OmniPixelImage image) {
    if (image.bytes == NULL || image.width == 0 || image.height == 0 || image.width > SIZE_MAX / 4U) {
        /* Input without one complete BGRA active row cannot enter pointer arithmetic. */
        return false;
    }
    size_t active = image.width * 4U;
    if (image.bytes_per_row < active || image.height - 1U > (SIZE_MAX - active) / image.bytes_per_row) {
        /* Reject short stride or cumulative offset overflow, without treating missing pixels as blank content. */
        return false;
    }
    size_t required = (image.height - 1U) * image.bytes_per_row + active;
    return required <= image.byte_count && required <= (size_t)PTRDIFF_MAX;
}

/* Subtraction bounds avoid x+width overflow; empty or out-of-bounds rectangles cannot participate through partial matching. */
static bool rect_inside(OmniPixelRect rect, size_t width, size_t height) {
    return rect.width != 0 && rect.height != 0 && rect.x <= width && rect.y <= height &&
           rect.width <= width - rect.x && rect.height <= height - rect.y;
}

/* A mask declares one validated finite rectangle; a disabled mask cannot hide nonzero alternate configuration. */
static bool mask_valid(OmniPixelMask mask, size_t width, size_t height) {
    if (mask.enabled) {
        /* An enabled mask must fit wholly within the ROI, without clipping it into another unknown region. */
        return rect_inside(mask.rect, width, height);
    }
    return mask.rect.x == 0 && mask.rect.y == 0 && mask.rect.width == 0 && mask.rect.height == 0;
}

/* Coordinates come from bounded ROI iteration; construction-time containment proves the rectangle additions cannot overflow. */
static bool masked(OmniPixelMask mask, size_t x, size_t y) {
    return mask.enabled && x >= mask.rect.x && x < mask.rect.x + mask.rect.width &&
           y >= mask.rect.y && y < mask.rect.y + mask.rect.height;
}

/* Copy only configured ROI active pixels, retaining neither the parent image, its padding, nor its provider. */
static void copy_roi(uint8_t *destination, OmniPixelImage image, OmniPixelRect roi) {
    size_t row_bytes = roi.width * 4U;
    for (size_t row = 0; row < roi.height; ++row) {
        /* Validated image/ROI bounds cover both copies, and the 128-row limit bounds iteration. */
        memcpy(destination + row * row_bytes,
               image.bytes + (roi.y + row) * image.bytes_per_row + roi.x * 4U,
               row_bytes);
    }
}

/* Failed construction releases only newly acquired storage; source images remain borrowed throughout. */
OmniPixelOracle *omni_pixel_oracle_create(OmniPixelConfig config,
                                        OmniPixelImage old_image,
                                        OmniPixelImage new_image,
                                        OmniPixelStatus *status) {
    OmniPixelStatus error = OMNI_PIXEL_OK;
    if (config.run_id == 0 || config.start_ticks == 0 || config.required_baseline_frames == 0 ||
        config.required_baseline_frames > 10U || config.frame_limit == 0 || config.frame_limit > OMNI_PIXEL_MAX_FRAMES) {
        /* Zero identity, missing origin, and unbounded counts cannot establish one finite sequence. */
        error = OMNI_PIXEL_INVALID_ARGUMENT;
    } else if (!image_valid(old_image) || !image_valid(new_image) || old_image.width != new_image.width ||
               old_image.height != new_image.height || !rect_inside(config.roi, old_image.width, old_image.height) ||
               config.roi.width > OMNI_PIXEL_MAX_WIDTH || config.roi.height > OMNI_PIXEL_MAX_HEIGHT ||
               !mask_valid(config.mask, config.roi.width, config.roi.height)) {
        /* Both source geometry and fixed mask must agree; different layouts cannot produce a guessed shared template. */
        error = OMNI_PIXEL_INVALID_GEOMETRY;
    } else if (config.roi.width * config.roi.height * 4U > OMNI_PIXEL_MAX_OWNED_BYTES / 3U) {
        /* Count both templates and the candidate in the actual pixel budget, rather than reporting a single crop. */
        error = OMNI_PIXEL_BUDGET_EXCEEDED;
    }
    if (error != OMNI_PIXEL_OK) {
        /* Rejected inputs allocate no owner and leave caller images unchanged. */
        if (status != NULL) {
            /* Error output is optional, but a provided slot receives the precise rejection reason. */
            *status = error;
        }
        return NULL;
    }
    OmniPixelOracle *oracle = calloc(1U, sizeof(*oracle));
    size_t bytes = config.roi.width * config.roi.height * 4U;
    if (oracle != NULL) {
        /* Allocate all three buffers once, reserving the candidate so observation grows neither pixels nor records. */
        oracle->pixels = malloc(bytes * 3U);
    }
    if (oracle == NULL || oracle->pixels == NULL) {
        /* Partial allocation must leave no new owner behind; source-pixel ownership was never transferred. */
        free(oracle);
        if (status != NULL) {
            /* Allocation failure cannot degrade into an empty template or partial comparison. */
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
        /* Verify original four-byte pixels, including masked pixels; thresholds cannot conceal template differences. */
        for (size_t x = 0; x < config.roi.width; ++x) {
            size_t offset = (y * config.roi.width + x) * 4U;
            bool equal = memcmp(oracle->pixels + offset, oracle->pixels + bytes + offset, 4U) == 0;
            if (masked(config.mask, x, y) && !equal) {
                /* Any masked difference rejects the whole configuration, without shrinking or moving its mask. */
                error = OMNI_PIXEL_MASK_HIDES_DIFFERENCE;
            }
            different |= !equal;
        }
    }
    if (!different && error == OMNI_PIXEL_OK) {
        /* Identical templates cannot distinguish OLD from NEW or establish a positive baseline. */
        error = OMNI_PIXEL_IDENTICAL_TEMPLATES;
    }
    if (error != OMNI_PIXEL_OK) {
        /* Release all three unpublished buffers on template failure, leaving no callable partial owner. */
        free(oracle->pixels);
        free(oracle);
        oracle = NULL;
    }
    if (status != NULL) {
        /* Publish an explicit result for success and failure, independent of the caller's previous status value. */
        *status = error;
    }
    return oracle;
}

/* Ignore only fixed regions with equal template bytes; no hash-only, color-tolerance, prefix, or shifted matching is used. */
OmniPixelStatus omni_pixel_oracle_classify(const OmniPixelOracle *oracle,
                                         OmniPixelImage image,
                                         OmniPixelState *state) {
    if (oracle == NULL || state == NULL) {
        /* A missing owner or output slot must not become a successful UNKNOWN classification. */
        return OMNI_PIXEL_INVALID_ARGUMENT;
    }
    if (oracle->sealed) {
        /* Finish may already have freed templates; late classification cannot access their old pixels. */
        return OMNI_PIXEL_SEALED;
    }
    if (!image_valid(image) || image.width != oracle->source_width || image.height != oracle->source_height) {
        /* Geometry changes fail rather than disguising replacement windows, scaling, or truncation as UNKNOWN content. */
        return OMNI_PIXEL_INVALID_GEOMETRY;
    }
    bool old_equal = true;
    bool new_equal = true;
    OmniPixelRect roi = oracle->config.roi;
    for (size_t y = 0; y < roi.height; ++y) {
        /* Read only the validated active row; callers keep source pixels stable throughout the call. */
        const uint8_t *source = image.bytes + (roi.y + y) * image.bytes_per_row + roi.x * 4U;
        for (size_t x = 0; x < roi.width; ++x) {
            if (masked(oracle->config.mask, x, y)) {
                /* Templates agree here; skipping this fixed region does not independently prove that it contains no text. */
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

/* Compare identity fields rather than struct bytes, excluding padding and layout from protocol equality. */
static bool binding_equal(OmniPixelAuditBinding a, OmniPixelAuditBinding b) {
    return a.run_id == b.run_id && a.frame_index == b.frame_index && a.display_ticks == b.display_ticks;
}

/* Each frame advances bounded state without waiting for audit; any failure prevents later witness certification. */
OmniPixelStatus omni_pixel_oracle_observe(OmniPixelOracle *oracle,
                                        OmniPixelFrame frame,
                                        OmniPixelImage image,
                                        OmniPixelRecord *record) {
    if (oracle == NULL) {
        /* An absent owner cannot participate in sequence advancement. */
        return OMNI_PIXEL_INVALID_ARGUMENT;
    }
    if (oracle->sealed) {
        /* The frozen footer rejects late callbacks without mutation or access to released templates. */
        return OMNI_PIXEL_SEALED;
    }
    if (record == NULL) {
        /* A missing record slot is an active-call failure; it cannot be ignored before certifying a later witness. */
        return fail(oracle, OMNI_PIXEL_INVALID_ARGUMENT);
    }
    if (oracle->failure != OMNI_PIXEL_OK) {
        /* Preserve the first failure; later normal frames cannot restore this observation. */
        return oracle->failure;
    }
    if (oracle->count == oracle->config.frame_limit) {
        /* A 601st frame, or any frame beyond the smaller declared limit, cannot overwrite earlier records. */
        return fail(oracle, OMNI_PIXEL_FRAME_OVERFLOW);
    }
    uint64_t last_display = oracle->count == 0 ? oracle->config.start_ticks : oracle->records[oracle->count - 1U].frame.display_ticks;
    uint64_t last_callback = oracle->count == 0 ? oracle->config.start_ticks : oracle->records[oracle->count - 1U].frame.callback_absolute_ticks;
    if (frame.run_id != oracle->config.run_id || frame.index != oracle->count + 1U ||
        frame.display_ticks <= last_display || frame.callback_absolute_ticks <= last_callback ||
        frame.callback_absolute_ticks < frame.display_ticks) {
        /* A wrong run, skipped index, duplicate, or backward timestamp cannot enter this observation or replace its first candidate. */
        return fail(oracle, OMNI_PIXEL_FRAME_ORDER);
    }
    OmniPixelState state = OMNI_PIXEL_UNKNOWN;
    OmniPixelStatus classified = omni_pixel_oracle_classify(oracle, image, &state);
    if (classified != OMNI_PIXEL_OK) {
        /* Format/layout failure differs from UNKNOWN; reject the observation without advancing accepted-frame count. */
        return fail(oracle, classified);
    }
    if (!oracle->baseline_ready && state == OMNI_PIXEL_NEW) {
        /* NEW before a positive OLD baseline is preexisting and cannot count as a fast input result. */
        return fail(oracle, OMNI_PIXEL_PREEXISTING_NEW);
    }
    if (state == OMNI_PIXEL_UNKNOWN) {
        /* Count UNKNOWN separately and interrupt consecutive OLD; uncertainty before the candidate remains permanent evidence. */
        oracle->unknown_count += 1U;
        if (!oracle->candidate_present) {
            /* A later audited NEW can establish an upper-bound witness, without proving that every earlier frame was OLD. */
            oracle->unknown_before_candidate += 1U;
        }
    }
    if (!oracle->baseline_ready) {
        /* Until baseline is established, require consecutive exact OLD images. */
        oracle->baseline_count = state == OMNI_PIXEL_OLD ? oracle->baseline_count + 1U : 0U;
        oracle->baseline_ready = oracle->baseline_count >= oracle->config.required_baseline_frames;
    }
    bool candidate = oracle->baseline_ready && state == OMNI_PIXEL_NEW && !oracle->candidate_present;
    if (candidate) {
        /* Copy candidate pixels and identity within this synchronous call; later classification or slow audit cannot replace them. */
        copy_roi(oracle->pixels + oracle->template_bytes * 2U, image, oracle->config.roi);
        oracle->candidate = (OmniPixelAuditBinding){frame.run_id, frame.index, frame.display_ticks};
        oracle->candidate_present = true;
    }
    OmniPixelRecord value = {frame, state, oracle->baseline_ready, candidate};
    oracle->records[oracle->count++] = value;
    *record = value;
    return OMNI_PIXEL_OK;
}

/* Lend only the copied candidate, requiring the caller to keep this owner alive until the audit loan returns. */
OmniPixelStatus omni_pixel_oracle_audit_begin(OmniPixelOracle *oracle,
                                            OmniPixelAuditBinding *binding,
                                            OmniPixelImage *candidate) {
    if (oracle == NULL) {
        /* No owner means no pixel loan can be created. */
        return OMNI_PIXEL_INVALID_ARGUMENT;
    }
    if (oracle->sealed) {
        /* Finished observations cannot start late audits or revive released templates. */
        return OMNI_PIXEL_SEALED;
    }
    if (binding == NULL || candidate == NULL) {
        /* Missing output slots are an active-owner failure, not an ignorable audit attempt. */
        return fail(oracle, OMNI_PIXEL_INVALID_ARGUMENT);
    }
    if (oracle->failure != OMNI_PIXEL_OK) {
        /* Failed observation cannot create more audit work. */
        return oracle->failure;
    }
    if (!oracle->candidate_present || oracle->audit_started) {
        /* Neither an absent candidate nor a second request can create a replacement for a failed or delayed audit. */
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

/* Consume external declarations only; boolean receipts do not mean this pure module performed Vision or native weak checks. */
OmniPixelStatus omni_pixel_oracle_audit_complete(OmniPixelOracle *oracle,
                                               OmniPixelAuditBinding binding,
                                               bool expected_validated,
                                               bool frame_graph_released) {
    if (oracle == NULL) {
        /* A receipt without an owner cannot identify any observation. */
        return OMNI_PIXEL_INVALID_ARGUMENT;
    }
    if (oracle->sealed) {
        /* Final results are frozen; late receipts cannot mutate audit facts, and pixel loans require separate return for cleanup. */
        return OMNI_PIXEL_SEALED;
    }
    if (!binding_equal(binding, oracle->candidate)) {
        /* Wrong identity cannot acquire the real candidate's authority; preserve its actual loan for the original audit's cleanup. */
        return fail(oracle, OMNI_PIXEL_AUDIT_IDENTITY);
    }
    if (!oracle->audit_started || oracle->audit_complete || !oracle->audit_loan) {
        /* Register an audit once; duplicate receipts or receipts without an acquired loan remain failures. */
        return fail(oracle, OMNI_PIXEL_AUDIT_STATE);
    }
    oracle->audit_complete = true;
    oracle->audit_passed = expected_validated && frame_graph_released;
    if (!oracle->audit_passed) {
        /* Missing external content or release evidence prevents witness certification; the loan still requires separate return. */
        (void)fail(oracle, OMNI_PIXEL_AUDIT_REJECTED);
    }
    // Work can return normally after an earlier observation failure; retain cleanup facts without returning a recovered OK.
    return oracle->failure;
}

/* Free only module-owned pixels, after every candidate loan has ended. */
static void release_pixels(OmniPixelOracle *oracle) {
    free(oracle->pixels);
    oracle->pixels = NULL;
    oracle->owned_pixel_bytes = 0;
}

/* Completion and loan return are separate, preventing free while asynchronous audit still reads the image. */
OmniPixelStatus omni_pixel_oracle_audit_release(OmniPixelOracle *oracle,
                                              OmniPixelAuditBinding binding) {
    if (oracle == NULL) {
        /* No owner means no loan can be returned. */
        return OMNI_PIXEL_INVALID_ARGUMENT;
    }
    if (!binding_equal(binding, oracle->candidate)) {
        /* A mismatched run/index/ticks cannot prematurely return the real candidate's loan. */
        return fail(oracle, OMNI_PIXEL_AUDIT_IDENTITY);
    }
    if (!oracle->audit_loan) {
        /* Duplicate return keeps ownership intact but prevents a still-active observation from succeeding. */
        return fail(oracle, OMNI_PIXEL_AUDIT_STATE);
    }
    oracle->audit_loan = false;
    OmniPixelStatus result = OMNI_PIXEL_OK;
    if (!oracle->audit_complete && !oracle->sealed) {
        /* Explicit abandonment permits cleanup, but loan return without an audit result cannot certify success. */
        result = fail(oracle, OMNI_PIXEL_AUDIT_STATE);
    }
    if (oracle->sealed) {
        /* Early finish already froze failure; late loan return completes cleanup without rewriting that footer. */
        release_pixels(oracle);
    }
    return result;
}

/* Freeze an observed upper-bound witness, never proof of input, first presentation, complete capture, or physical scanout. */
OmniPixelFooter omni_pixel_oracle_finish(OmniPixelOracle *oracle) {
    if (oracle == NULL) {
        /* An unsuccessfully created owner can only produce an explicitly invalid result. */
        return (OmniPixelFooter){.failure = OMNI_PIXEL_INVALID_ARGUMENT};
    }
    if (oracle->sealed) {
        /* Late events and audits may clean up, but cannot make a previously invalid final record valid. */
        return oracle->footer;
    }
    oracle->sealed = true;
    if (!oracle->candidate_present) {
        /* OLD and UNKNOWN images cannot establish a NEW witness. */
        (void)fail(oracle, OMNI_PIXEL_NO_WITNESS);
    } else if (!oracle->audit_complete || oracle->audit_loan) {
        /* Finish audit and all pixel loans before certifying a footer; active work cannot support completion. */
        (void)fail(oracle, OMNI_PIXEL_AUDIT_PENDING);
    }
    if (!oracle->audit_loan) {
        /* With no external pixel loans, free all template/candidate capacity while retaining exportable scalar records. */
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

/* Export one scalar copy, exposing neither the internal array nor an image owner. */
bool omni_pixel_oracle_record(const OmniPixelOracle *oracle, uint32_t offset, OmniPixelRecord *record) {
    if (oracle == NULL || record == NULL || offset >= oracle->count) {
        /* An invalid range leaves output unchanged and cannot present unused capacity as a record. */
        return false;
    }
    *record = oracle->records[offset];
    return true;
}

/* Report actual module-owned pixels, without estimating system or Vision-internal caches. */
size_t omni_pixel_oracle_owned_pixel_bytes(const OmniPixelOracle *oracle) {
    return oracle == NULL ? 0 : oracle->owned_pixel_bytes;
}

/* Scalar bytes include the owner layout and 600 fixed records, with no separate growable list. */
size_t omni_pixel_oracle_owned_scalar_bytes(const OmniPixelOracle *oracle) {
    return oracle == NULL ? 0 : sizeof(*oracle);
}

/* Destruction performs no audit or I/O; outstanding loans preserve the real owner until callers finish their work. */
OmniPixelStatus omni_pixel_oracle_destroy(OmniPixelOracle **oracle) {
    if (oracle == NULL) {
        /* Without the caller's owner slot, release responsibility cannot transfer safely. */
        return OMNI_PIXEL_INVALID_ARGUMENT;
    }
    if (*oracle == NULL) {
        /* Idempotent cleanup of an empty slot creates no state. */
        return OMNI_PIXEL_OK;
    }
    if ((*oracle)->audit_loan) {
        /* Preserve this failure too; a later proper loan return permits cleanup without repairing the invalid early destruction. */
        return fail(*oracle, OMNI_PIXEL_AUDIT_PENDING);
    }
    release_pixels(*oracle);
    free(*oracle);
    *oracle = NULL;
    return OMNI_PIXEL_OK;
}
