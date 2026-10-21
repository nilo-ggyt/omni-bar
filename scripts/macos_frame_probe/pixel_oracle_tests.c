/* Pure contract tests use owned synthetic bytes, without reading screens, processes, files, clocks, or user content. */
#include "pixel_oracle.h"

#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static unsigned int checks;
static unsigned int cases;
static size_t observed_scalar_bytes;
static size_t peak_owned_pixel_bytes;

/* Assertions emit fixed labels only; immediate exit on failure prevents a later false passing summary. */
static void require(bool value, const char *label) {
    checks += 1U;
    if (!value) {
        /* Labels come from this source, keeping pixel bytes and recognized text out of diagnostics. */
        fprintf(stderr, "pixel oracle assertion failed: %s\n", label);
        exit(EXIT_FAILURE);
    }
}

/* A small image includes row padding and a nonzero ROI origin, exercising more than compact whole-buffer input. */
typedef struct {
    uint8_t old_bytes[160];
    uint8_t new_bytes[160];
    uint8_t current[160];
    OmniPixelConfig config;
} Fixture;

/* Initialize each fixture independently, sharing neither mutable owners nor loans. */
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

/* Borrow the complete row extent; padding remains outside the actual ROI pixels. */
static OmniPixelImage image(const uint8_t *bytes) {
    return (OmniPixelImage){bytes, 160, 8, 4, 40};
}

/* Successful construction reports actual three-buffer pixel capacity and fixed scalar capacity. */
static OmniPixelOracle *create(Fixture *f) {
    OmniPixelStatus status = OMNI_PIXEL_ALLOCATION_FAILED;
    OmniPixelOracle *oracle = omni_pixel_oracle_create(f->config, image(f->old_bytes), image(f->new_bytes), &status);
    require(oracle != NULL && status == OMNI_PIXEL_OK, "fixture owner construction");
    require(omni_pixel_oracle_owned_pixel_bytes(oracle) == 4U * 2U * 4U * 3U, "three actual compact allocations counted");
    require(omni_pixel_oracle_owned_scalar_bytes(oracle) >= sizeof(OmniPixelRecord) * OMNI_PIXEL_MAX_FRAMES,
            "all 600 scalar slots included");
    observed_scalar_bytes = omni_pixel_oracle_owned_scalar_bytes(oracle);
    if (omni_pixel_oracle_owned_pixel_bytes(oracle) > peak_owned_pixel_bytes) {
        /* Aggregate actual owner counts, without presenting the configured ceiling as measured allocation. */
        peak_owned_pixel_bytes = omni_pixel_oracle_owned_pixel_bytes(oracle);
    }
    return oracle;
}

/* Frames use explicit synthetic integer ticks, without claiming real Mach or SCK timestamps. */
static OmniPixelFrame frame(uint32_t index) {
    return (OmniPixelFrame){17, index, 1000U + (uint64_t)index * 16667U,
                            1100U + (uint64_t)index * 16667U};
}

/* Accepted input also requires output identity for this frame, excluding a cached previous record. */
static OmniPixelRecord observe(OmniPixelOracle *oracle, uint32_t index, const uint8_t *bytes) {
    OmniPixelRecord record = {0};
    require(omni_pixel_oracle_observe(oracle, frame(index), image(bytes), &record) == OMNI_PIXEL_OK,
            "admit one explicit synthetic frame");
    require(record.frame.index == index && record.frame.display_ticks == frame(index).display_ticks,
            "record retains this frame identity");
    return record;
}

/* After loan return, destruction must clear the caller's owner slot instead of merely changing a count. */
static void destroy(OmniPixelOracle **oracle) {
    require(omni_pixel_oracle_destroy(oracle) == OMNI_PIXEL_OK, "owned cleanup succeeds");
    require(*oracle == NULL, "owned cleanup empties caller slot");
}

/* Audit completion and loan return are distinct; supplied booleans are not proof of real Vision execution. */
static void finish_audit(OmniPixelOracle *oracle, OmniPixelAuditBinding binding, OmniPixelStatus expected) {
    require(omni_pixel_oracle_audit_complete(oracle, binding, true, true) == expected,
            "synthetic audit returns expected original failure or success");
    require(omni_pixel_oracle_audit_release(oracle, binding) == OMNI_PIXEL_OK, "audit loan explicitly returned");
}

/* Without a prior failure, a supplied successful audit and explicit loan return finish candidate work. */
static void pass_audit(OmniPixelOracle *oracle, OmniPixelAuditBinding binding) {
    finish_audit(oracle, binding, OMNI_PIXEL_OK);
}

/* Failed construction leaves no owner and preserves the specific rejection reason. */
static void rejected(OmniPixelConfig config, OmniPixelImage old, OmniPixelImage new_image, OmniPixelStatus expected) {
    OmniPixelStatus status = OMNI_PIXEL_OK;
    OmniPixelOracle *oracle = omni_pixel_oracle_create(config, old, new_image, &status);
    require(oracle == NULL && status == expected, "invalid construction rejects exact boundary");
}

/* Pixel counterexamples cover the full ROI, all channels, row padding, source copying, and UNKNOWN classification. */
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
        /* Flipping any ROI bit, including alpha, must prevent approximate-match acceptance. */
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

/* Invalid sizes and multiplication boundaries must reject before reading source bytes. */
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
        /* Test each count boundary independently so another invalid field cannot mask its rejection. */
        bad = f.config;
        if (kind < 2U) {
            /* Baseline accepts only 1..10 frames. */
            bad.required_baseline_frames = kind == 0 ? 0 : 11;
        } else {
            /* Frame budget accepts only 1..600 records. */
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
        /* An oversized byte_count or padding claim cannot make invalid source geometry readable. */
        OmniPixelImage broken = old;
        switch (kind) {
            case 0: /* Null bytes provide no readable source. */
                broken.bytes = NULL;
                break;
            case 1: /* Even one missing byte in the last active row must fail. */
                broken.byte_count = 151;
                break;
            case 2: /* Stride cannot be shorter than one complete active row. */
                broken.bytes_per_row = 31;
                break;
            case 3: /* Reject width-times-four overflow before multiplication. */
                broken.width = SIZE_MAX;
                break;
            case 4: /* Overflowing cumulative stride must not produce a pointer. */
                broken.bytes_per_row = SIZE_MAX;
                break;
            case 5: /* A different source height cannot inherit reference geometry. */
                broken.height = 3;
                break;
            default: /* An empty image is not blank OLD evidence. */
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

/* A mask allows fixed common pixels to vary, without leaving the ROI or hiding even one OLD/NEW difference. */
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

/* Three actual buffers cannot evade the combined budget by reporting a single crop's size. */
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

/* UNKNOWN interrupts baseline; later audit success establishes a witness without proving earlier absence of NEW. */
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

/* Defer audit through a 600-frame replay; classification must progress without OCR completion or overwriting old frames. */
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
        /* Repeated NEW/OLD transitions and later UNKNOWN cannot move the candidate nearer audit completion. */
        memcpy(f.current, index % 2U == 0 ? f.new_bytes : f.old_bytes, sizeof(f.current));
        if (index == 99) {
            /* Later UNKNOWN preserves the earlier all-OLD fact while keeping whole-sequence classification uncertainty. */
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
        /* Even an audited candidate cannot excuse a later frame beyond the declared budget. */
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

/* Sequence errors fail before record publication, preserving the original candidate and accepted records. */
static void ordering_failures(void) {
    cases += 1U;
    for (unsigned int kind = 0; kind < 7; ++kind) {
        /* Use a fresh owner per case so an earlier failure cannot mask another boundary. */
        Fixture f = fixture();
        OmniPixelOracle *oracle = create(&f);
        (void)observe(oracle, 1, f.old_bytes);
        OmniPixelFrame bad = frame(2);
        switch (kind) {
            case 0: /* Another run's frame cannot enter this sequence. */
                bad.run_id += 1U;
                break;
            case 1: /* A repeated index cannot count as a new frame. */
                bad.index = 1;
                break;
            case 2: /* A gap in the supplied sequence must explicitly fail. */
                bad.index = 3;
                break;
            case 3: /* Repeated display ticks provide no new presentation evidence. */
                bad.display_ticks = frame(1).display_ticks;
                break;
            case 4: /* Moving before the origin must also fail. */
                bad.display_ticks = 999;
                break;
            case 5: /* OCR timing cannot conceal backward callback clocks. */
                bad.callback_absolute_ticks = frame(1).callback_absolute_ticks;
                break;
            default: /* Callback before its declared display time is inconsistent clock input. */
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

/* Preexisting NEW and candidate-free OLD/UNKNOWN sequences cannot establish a witness. */
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

/* An audit with different run/index/ticks cannot acquire authority over the actual candidate. */
static void mismatched_audits(void) {
    cases += 1U;
    for (unsigned int kind = 0; kind < 3; ++kind) {
        /* Change each identity coordinate independently, rather than protecting only the simplest run check. */
        Fixture f = fixture();
        OmniPixelOracle *oracle = create(&f);
        (void)observe(oracle, 1, f.old_bytes);
        (void)observe(oracle, 2, f.new_bytes);
        OmniPixelAuditBinding binding = {0};
        OmniPixelImage candidate = {0};
        require(omni_pixel_oracle_audit_begin(oracle, &binding, &candidate) == OMNI_PIXEL_OK, "begin identity test audit");
        OmniPixelAuditBinding bad = binding;
        if (kind == 0) {
            /* The same frame index from another run still cannot match. */
            bad.run_id += 1U;
        } else if (kind == 1) {
            /* A later frame in this run cannot backfill the first candidate. */
            bad.frame_index += 1U;
        } else {
            /* Different ticks alone still identify another candidate. */
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

/* Neither rejected content nor a false release declaration can be replaced by a second successful receipt. */
static void rejected_and_duplicate_audits(void) {
    cases += 1U;
    for (unsigned int kind = 0; kind < 3; ++kind) {
        /* Only the third case supplies initial success before sending an invalid duplicate receipt. */
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

/* Early finish or destruction must retain a borrowed candidate; late work can only return the loan. */
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

/* Smaller caps, layout changes, and unaudited loan returns must fail explicitly rather than silently skipping frames. */
static void additional_failure_edges(void) {
    cases += 1U;
    for (unsigned int kind = 0; kind < 4; ++kind) {
        /* Each lifecycle error starts with its own independent two-frame candidate. */
        Fixture f = fixture();
        f.config.frame_limit = 2;
        OmniPixelOracle *oracle = create(&f);
        (void)observe(oracle, 1, f.old_bytes);
        (void)observe(oracle, 2, f.new_bytes);
        OmniPixelAuditBinding binding = {0};
        OmniPixelImage candidate = {0};
        require(omni_pixel_oracle_audit_begin(oracle, &binding, &candidate) == OMNI_PIXEL_OK, "begin additional edge audit");
        if (kind == 0) {
            /* Work may abandon its result, but loan return alone cannot establish successful audit. */
            require(omni_pixel_oracle_audit_release(oracle, binding) == OMNI_PIXEL_AUDIT_STATE, "release without result is abandonment");
        } else if (kind == 1) {
            /* Invalid early destruction preserves failure even before the footer exists. */
            require(omni_pixel_oracle_destroy(&oracle) == OMNI_PIXEL_AUDIT_PENDING, "early destruction rejected");
            finish_audit(oracle, binding, OMNI_PIXEL_AUDIT_PENDING);
        } else if (kind == 2) {
            /* A second task acquires neither another candidate nor authority to clear the first loan. */
            OmniPixelAuditBinding ignored_binding = {0};
            OmniPixelImage ignored_image = {0};
            require(omni_pixel_oracle_audit_begin(oracle, &ignored_binding, &ignored_image) == OMNI_PIXEL_AUDIT_STATE,
                    "only one audit job admitted");
            finish_audit(oracle, binding, OMNI_PIXEL_AUDIT_STATE);
        } else {
            /* A smaller declared frame limit must also prevent accepted records from being overwritten. */
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

/* Ticks near u64 maximum remain exact, and calls after finish cannot change frozen records. */
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

/* Active-call errors cannot disappear silently; UNKNOWN-only or unaudited observations cannot pass. */
static void missing_audits_and_arguments(void) {
    cases += 1U;
    for (unsigned int kind = 0; kind < 5; ++kind) {
        /* Do not share failed state, keeping each failure source independently reachable. */
        Fixture f = fixture();
        OmniPixelOracle *oracle = create(&f);
        if (kind == 4) {
            /* UNKNOWN alone provides positive evidence of neither OLD nor NEW. */
            f.current[1U * 40U + 2U * 4U] ^= 1U;
            require(observe(oracle, 1, f.current).state == OMNI_PIXEL_UNKNOWN, "unknown-only sample recorded");
        } else {
            /* Other cases begin with a normal candidate; prior success cannot excuse a later invalid call. */
            (void)observe(oracle, 1, f.old_bytes);
            (void)observe(oracle, 2, f.new_bytes);
            OmniPixelAuditBinding binding = {0};
            OmniPixelImage candidate = {0};
            if (kind == 0) {
                /* Missing record output violates the active sequencing API contract. */
                require(omni_pixel_oracle_observe(oracle, frame(3), image(f.new_bytes), NULL) == OMNI_PIXEL_INVALID_ARGUMENT,
                        "missing active record output fails closed");
            } else if (kind == 1) {
                /* Missing audit output cannot become an ignored invocation. */
                require(omni_pixel_oracle_audit_begin(oracle, NULL, &candidate) == OMNI_PIXEL_INVALID_ARGUMENT,
                        "missing audit binding output fails closed");
            } else if (kind == 2) {
                /* An unaudited candidate makes finish fail and release every pixel buffer with no outstanding loan. */
                require(omni_pixel_oracle_owned_pixel_bytes(oracle) != 0, "unaudited candidate still owned before finish");
            } else {
                /* Duplicate return after normal audit still violates protocol and cannot invent another success. */
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

/* Compact source stride remains valid; copy active ROI pixels without incorporating padding into templates. */
static void different_valid_strides(void) {
    cases += 1U;
    Fixture f = fixture();
    OmniPixelOracle *oracle = create(&f);
    uint8_t compact[128];
    for (size_t row = 0; row < 4; ++row) {
        /* Copy active rows from a padded reference into independently owned compact memory. */
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
        /* Every compact candidate row must equal the source ROI at capture, rather than checking one distinctive byte. */
        require(memcmp(candidate.bytes + row * 16U, compact + (row + 1U) * 32U + 8U, 16U) == 0,
                "all candidate ROI rows are exact independent copies");
    }
    memset(compact, 0, sizeof(compact));
    require(candidate.bytes[4] == 0xaaU, "source mutation cannot change same-frame candidate");
    pass_audit(oracle, binding);
    require(omni_pixel_oracle_finish(oracle).witness_valid, "packed witness completes without source ownership");
    destroy(&oracle);
}

/* Emit a scalar summary only after all pure cases succeed, without invoking capture or permission APIs. */
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
