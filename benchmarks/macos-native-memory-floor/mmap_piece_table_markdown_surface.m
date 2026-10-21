#import <Cocoa/Cocoa.h>

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

// Historical standalone piece-table floor comparison, not the production Markdown plugin.
// It has simplified monospaced painting, navigation and lifecycle semantics. Only caller-supplied
// disposable fixtures may be used for diagnostics; it has no authority over the production vault.
// The product's strict paging, shaping, undo and release evidence comes from its actual Rust path.
static const NSUInteger kMaximumMarkdownFileBytes = 16 * 1024 * 1024;
static const NSUInteger kOriginalMarkdownPieceBytes = 64 * 1024;
static const NSUInteger kMaximumViewportUTF8Bytes = 256 * 1024;
static const NSUInteger kMaximumInputContextUTF8Bytes = 16 * 1024;

typedef NS_ENUM(uint8_t, OmniMarkdownPieceSource) {
  OmniMarkdownPieceSourceOriginalFile = 0,
  OmniMarkdownPieceSourceSessionAddition = 1,
};

typedef struct {
  OmniMarkdownPieceSource source;
  NSUInteger sourceOffset;
  NSUInteger byteLength;
  NSUInteger utf16Length;
  NSUInteger newlineCount;
} OmniMarkdownPiece;

typedef struct {
  NSUInteger utf16Length;
  NSUInteger newlineCount;
} OmniMarkdownTextMetrics;

typedef struct {
  void *base;
  size_t mappedLength;
  const uint8_t *bytes;
} OmniMarkdownFileMapping;

static BOOL markdown_file_revision_matches(
    int descriptor, const char *path, NSUInteger expected_byte_length,
    dev_t expected_device, ino_t expected_inode,
    struct timespec expected_modification_time) {
  // Reconcile the retained descriptor with the current real pathname before borrowing original bytes.
  // A missing/replaced node or changed size/time cannot be accepted as the originally opened source.
  struct stat descriptor_status;
  struct stat path_status;
  if (descriptor < 0 || path == NULL ||
      fstat(descriptor, &descriptor_status) != 0 ||
      lstat(path, &path_status) != 0 ||
      !S_ISREG(descriptor_status.st_mode) || !S_ISREG(path_status.st_mode)) {
    return NO;
  }
  return descriptor_status.st_dev == expected_device &&
         descriptor_status.st_ino == expected_inode &&
         descriptor_status.st_size == (off_t)expected_byte_length &&
         descriptor_status.st_mtimespec.tv_sec ==
             expected_modification_time.tv_sec &&
         descriptor_status.st_mtimespec.tv_nsec ==
             expected_modification_time.tv_nsec &&
         path_status.st_dev == expected_device &&
         path_status.st_ino == expected_inode &&
         path_status.st_size == (off_t)expected_byte_length &&
         path_status.st_mtimespec.tv_sec == expected_modification_time.tv_sec &&
         path_status.st_mtimespec.tv_nsec == expected_modification_time.tv_nsec;
}

static NSError *omni_markdown_model_error(NSInteger code,
                                          NSString *description) {
  // Errors belong to this comparison domain and never map to product or SDK error identities.
  return [NSError errorWithDomain:@"com.omni.native-markdown-floor"
                             code:code
                         userInfo:@{NSLocalizedDescriptionKey : description}];
}

static BOOL decode_markdown_utf8_scalar(const uint8_t *bytes, NSUInteger length,
                                        NSUInteger offset,
                                        NSUInteger *scalarByteLength,
                                        NSUInteger *scalarUTF16Length) {
  // Decode one exact scalar while counting both UTF-8 bytes and AppKit UTF-16 units.
  if (offset >= length) {
    return NO;
  }

  uint8_t first = bytes[offset];
  if (first <= 0x7f) {
    *scalarByteLength = 1;
    *scalarUTF16Length = 1;
    return YES;
  }

  NSUInteger width = 0;
  // Admit only canonical lead-byte ranges; continuation bytes cannot start a scalar.
  if (first >= 0xc2 && first <= 0xdf) {
    width = 2;
  } else if (first >= 0xe0 && first <= 0xef) {
    width = 3;
  } else if (first >= 0xf0 && first <= 0xf4) {
    width = 4;
  } else {
    return NO;
  }
  if (width > length - offset) {
    return NO;
  }
  for (NSUInteger continuation = 1; continuation < width; continuation += 1) {
    // Every required continuation must remain inside the caller's live byte slice.
    if ((bytes[offset + continuation] & 0xc0) != 0x80) {
      return NO;
    }
  }
  if ((first == 0xe0 && bytes[offset + 1] < 0xa0) ||
      (first == 0xed && bytes[offset + 1] >= 0xa0) ||
      (first == 0xf0 && bytes[offset + 1] < 0x90) ||
      (first == 0xf4 && bytes[offset + 1] >= 0x90)) {
    // Exclude overlong encodings, surrogate scalars and values beyond Unicode's maximum.
    return NO;
  }

  *scalarByteLength = width;
  *scalarUTF16Length = width == 4 ? 2 : 1;
  return YES;
}

static BOOL measure_markdown_utf8(const uint8_t *bytes, NSUInteger length,
                                  OmniMarkdownTextMetrics *metrics) {
  // Accumulate scalar/line metadata without allocating a decoded whole-string mirror.
  NSUInteger offset = 0;
  NSUInteger utf16_length = 0;
  NSUInteger newline_count = 0;
  while (offset < length) {
    NSUInteger scalar_byte_length = 0;
    NSUInteger scalar_utf16_length = 0;
    if (!decode_markdown_utf8_scalar(bytes, length, offset,
                                     &scalar_byte_length,
                                     &scalar_utf16_length)) {
      return NO;
    }
    if (bytes[offset] == '\n') {
      newline_count += 1;
    }
    offset += scalar_byte_length;
    utf16_length += scalar_utf16_length;
  }
  metrics->utf16Length = utf16_length;
  metrics->newlineCount = newline_count;
  return YES;
}

static BOOL markdown_byte_offset_for_utf16(const uint8_t *bytes,
                                           NSUInteger length,
                                           NSUInteger requestedUTF16Offset,
                                           NSUInteger *byteOffset) {
  // Translate an exact UTF-16 boundary; landing inside an astral scalar is a rejected edit/read.
  NSUInteger current_byte = 0;
  NSUInteger current_utf16 = 0;
  while (current_byte < length && current_utf16 < requestedUTF16Offset) {
    NSUInteger scalar_byte_length = 0;
    NSUInteger scalar_utf16_length = 0;
    if (!decode_markdown_utf8_scalar(bytes, length, current_byte,
                                     &scalar_byte_length,
                                     &scalar_utf16_length) ||
        scalar_utf16_length > requestedUTF16Offset - current_utf16) {
      return NO;
    }
    current_byte += scalar_byte_length;
    current_utf16 += scalar_utf16_length;
  }
  if (current_utf16 != requestedUTF16Offset) {
    return NO;
  }
  *byteOffset = current_byte;
  return YES;
}

static void append_markdown_piece(NSMutableData *pieceStorage,
                                  OmniMarkdownPiece piece) {
  // Empty edits create no piece. Adjacent source spans merge without copying their source bytes.
  if (piece.byteLength == 0 && piece.utf16Length == 0) {
    return;
  }
  NSUInteger piece_count = pieceStorage.length / sizeof(OmniMarkdownPiece);
  if (piece_count > 0) {
    OmniMarkdownPiece *pieces = pieceStorage.mutableBytes;
    OmniMarkdownPiece *previous = &pieces[piece_count - 1];
    if (previous->source == piece.source &&
        previous->sourceOffset + previous->byteLength == piece.sourceOffset) {
      previous->byteLength += piece.byteLength;
      previous->utf16Length += piece.utf16Length;
      previous->newlineCount += piece.newlineCount;
      return;
    }
  }
  [pieceStorage appendBytes:&piece length:sizeof(piece)];
}

@interface OmniMappedMarkdownDocument : NSObject {
  int _originalFileDescriptor;
  NSUInteger _originalByteLength;
  dev_t _originalDevice;
  ino_t _originalInode;
  struct timespec _originalModificationTime;
  NSData *_originalPathBytes;
  NSMutableData *_sessionAdditionBytes;
  NSMutableData *_pieceStorage;
  NSUInteger _utf16Length;
  NSUInteger _lineCount;
}

@property(nonatomic, readonly) NSUInteger utf16Length;
@property(nonatomic, readonly) NSUInteger lineCount;

- (nullable instancetype)initWithFileSystemRepresentation:(const char *)path
                                                     error:(NSError **)error;
- (BOOL)replaceUTF16Range:(NSRange)range
                  withText:(NSString *)text
                     error:(NSError **)error;
- (nullable NSString *)stringForUTF16Range:(NSRange)range
                                byteBudget:(NSUInteger)byteBudget
                         actualUTF16Length:(NSUInteger *)actualUTF16Length
                                     error:(NSError **)error;
- (NSUInteger)utf16OffsetForLine:(NSUInteger)line;
- (void)getLine:(NSUInteger *)line
      utf16Column:(NSUInteger *)column
    forUTF16Offset:(NSUInteger)offset;
@end

@implementation OmniMappedMarkdownDocument

- (nullable instancetype)initWithFileSystemRepresentation:(const char *)path
                                                     error:(NSError **)error {
  // Retain one original read-only descriptor and append-only edit storage for this standalone session.
  self = [super init];
  if (self == nil) {
    return nil;
  }
  _originalFileDescriptor = -1;
  _sessionAdditionBytes = [[NSMutableData alloc] init];
  _pieceStorage = [[NSMutableData alloc] init];

  int descriptor = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
  // A final-component symlink is not an original-file authority; failed acquisition owns no descriptor.
  if (descriptor < 0) {
    if (error != NULL) {
      *error = omni_markdown_model_error(1, @"The Markdown file could not be opened.");
    }
    return nil;
  }

  struct stat file_status;
  // Bound this historical control to finite regular files before it constructs any mapping or index.
  if (fstat(descriptor, &file_status) != 0 ||
      !S_ISREG(file_status.st_mode) || file_status.st_size < 0 ||
      (uint64_t)file_status.st_size > (uint64_t)kMaximumMarkdownFileBytes) {
    close(descriptor);
    if (error != NULL) {
      *error = omni_markdown_model_error(
          2, @"The Markdown file is not a supported regular file.");
    }
    return nil;
  }

  _originalFileDescriptor = descriptor;
  // Transfer the descriptor only after its initial type/size checks, then reconcile its pathname identity.
  _originalByteLength = (NSUInteger)file_status.st_size;
  _originalDevice = file_status.st_dev;
  _originalInode = file_status.st_ino;
  _originalModificationTime = file_status.st_mtimespec;
  _originalPathBytes = [[NSData alloc] initWithBytes:path
                                              length:strlen(path) + 1];
  if (!markdown_file_revision_matches(
          _originalFileDescriptor, _originalPathBytes.bytes,
          _originalByteLength, _originalDevice, _originalInode,
          _originalModificationTime)) {
    close(_originalFileDescriptor);
    _originalFileDescriptor = -1;
    if (error != NULL) {
      *error = omni_markdown_model_error(
          3, @"The Markdown file changed while the editor session opened.");
    }
    return nil;
  }

  if (_originalByteLength == 0) {
    // An empty source needs one logical line but no zero-length mmap, which the kernel rejects.
    _utf16Length = 0;
    _lineCount = 1;
    return self;
  }

  // This old comparison scans a temporary full virtual mapping to build metadata. It is not
  // the production plugin's bounded mapped-window/index implementation or its memory proof.
  void *mapped_file = mmap(NULL, _originalByteLength, PROT_READ, MAP_PRIVATE,
                           _originalFileDescriptor, 0);
  if (mapped_file == MAP_FAILED) {
    close(_originalFileDescriptor);
    _originalFileDescriptor = -1;
    if (error != NULL) {
      *error = omni_markdown_model_error(4, @"The Markdown file could not be mapped.");
    }
    return nil;
  }
  (void)madvise(mapped_file, _originalByteLength, MADV_SEQUENTIAL);

  const uint8_t *bytes = mapped_file;
  NSUInteger byte_offset = 0;
  NSUInteger piece_start = 0;
  OmniMarkdownTextMetrics piece_metrics = {0, 0};
  BOOL valid_utf8 = YES;
  // Record scalar-complete original spans; no borrowed pointer survives the temporary mapping below.
  while (byte_offset < _originalByteLength) {
    NSUInteger scalar_byte_length = 0;
    NSUInteger scalar_utf16_length = 0;
    if (!decode_markdown_utf8_scalar(bytes, _originalByteLength, byte_offset,
                                     &scalar_byte_length,
                                     &scalar_utf16_length)) {
      valid_utf8 = NO;
      break;
    }
    if (bytes[byte_offset] == '\n') {
      piece_metrics.newlineCount += 1;
    }
    byte_offset += scalar_byte_length;
    piece_metrics.utf16Length += scalar_utf16_length;
    if (byte_offset - piece_start >= kOriginalMarkdownPieceBytes) {
      OmniMarkdownPiece piece = {
          .source = OmniMarkdownPieceSourceOriginalFile,
          .sourceOffset = piece_start,
          .byteLength = byte_offset - piece_start,
          .utf16Length = piece_metrics.utf16Length,
          .newlineCount = piece_metrics.newlineCount,
      };
      append_markdown_piece(_pieceStorage, piece);
      piece_start = byte_offset;
      piece_metrics = (OmniMarkdownTextMetrics){0, 0};
    }
  }
  if (valid_utf8 && byte_offset > piece_start) {
    OmniMarkdownPiece final_piece = {
        .source = OmniMarkdownPieceSourceOriginalFile,
        .sourceOffset = piece_start,
        .byteLength = byte_offset - piece_start,
        .utf16Length = piece_metrics.utf16Length,
        .newlineCount = piece_metrics.newlineCount,
    };
    append_markdown_piece(_pieceStorage, final_piece);
  }

  BOOL revision_unchanged = markdown_file_revision_matches(
      _originalFileDescriptor, _originalPathBytes.bytes, _originalByteLength,
      _originalDevice, _originalInode, _originalModificationTime);
  // Release the scan mapping before returning either malformed-source or changed-revision failure.
  (void)madvise(mapped_file, _originalByteLength, MADV_DONTNEED);
  munmap(mapped_file, _originalByteLength);
  if (!revision_unchanged) {
    close(_originalFileDescriptor);
    _originalFileDescriptor = -1;
    if (error != NULL) {
      *error = omni_markdown_model_error(
          3, @"The Markdown file changed while the editor session opened.");
    }
    return nil;
  }
  if (!valid_utf8) {
    close(_originalFileDescriptor);
    _originalFileDescriptor = -1;
    if (error != NULL) {
      *error = omni_markdown_model_error(5, @"The Markdown file is not valid UTF-8.");
    }
    return nil;
  }

  const OmniMarkdownPiece *pieces = _pieceStorage.bytes;
  // Sum only stored piece metadata, preserving one terminal line after the final newline.
  NSUInteger piece_count = _pieceStorage.length / sizeof(OmniMarkdownPiece);
  NSUInteger newline_count = 0;
  for (NSUInteger index = 0; index < piece_count; index += 1) {
    _utf16Length += pieces[index].utf16Length;
    newline_count += pieces[index].newlineCount;
  }
  _lineCount = newline_count + 1;
  return self;
}

- (void)dealloc {
  // The document alone owns the original descriptor; transient range mappings have separate callers.
  if (_originalFileDescriptor >= 0) {
    close(_originalFileDescriptor);
  }
}

- (NSUInteger)utf16Length {
  // AppKit coordinates refer to the edited logical sequence, not original UTF-8 byte offsets.
  return _utf16Length;
}

- (NSUInteger)lineCount {
  // This comparison counts LF line boundaries; it does not implement production block/layout semantics.
  return _lineCount;
}

- (BOOL)mapOriginalRange:(NSRange)range
                  mapping:(OmniMarkdownFileMapping *)mapping
                    error:(NSError **)error {
  // Initialize an empty lease so every error leaves the caller with nothing to unmap.
  mapping->base = NULL;
  mapping->mappedLength = 0;
  mapping->bytes = NULL;
  if (range.length == 0) {
    return YES;
  }
  if (_originalFileDescriptor < 0 || range.location > _originalByteLength ||
      range.length > _originalByteLength - range.location) {
    if (error != NULL) {
      *error = omni_markdown_model_error(6, @"The mapped Markdown range is invalid.");
    }
    return NO;
  }

  if (!markdown_file_revision_matches(
          _originalFileDescriptor, _originalPathBytes.bytes,
          _originalByteLength, _originalDevice, _originalInode,
          _originalModificationTime)) {
    if (error != NULL) {
      *error = omni_markdown_model_error(
          7, @"The Markdown file changed outside this editor session.");
    }
    return NO;
  }

  long page_size_value = sysconf(_SC_PAGESIZE);
  // Kernel mapping offsets must be page-aligned; retain the leading-byte displacement separately.
  if (page_size_value <= 0) {
    if (error != NULL) {
      *error = omni_markdown_model_error(8, @"The virtual-memory page size is unavailable.");
    }
    return NO;
  }
  NSUInteger page_size = (NSUInteger)page_size_value;
  NSUInteger aligned_offset = range.location - (range.location % page_size);
  NSUInteger leading_bytes = range.location - aligned_offset;
  NSUInteger mapped_length = leading_bytes + range.length;
  void *base = mmap(NULL, mapped_length, PROT_READ, MAP_PRIVATE,
                    _originalFileDescriptor, (off_t)aligned_offset);
  if (base == MAP_FAILED) {
    if (error != NULL) {
      *error = omni_markdown_model_error(9, @"The Markdown viewport could not be mapped.");
    }
    return NO;
  }
  if (!markdown_file_revision_matches(
          _originalFileDescriptor, _originalPathBytes.bytes,
          _originalByteLength, _originalDevice, _originalInode,
          _originalModificationTime)) {
    munmap(base, mapped_length);
    // A revision change during acquisition revokes this new lease before any source pointer escapes.
    if (error != NULL) {
      *error = omni_markdown_model_error(
          10, @"The Markdown file changed while the viewport was mapped.");
    }
    return NO;
  }
  (void)madvise(base, mapped_length, MADV_RANDOM);
  mapping->base = base;
  mapping->mappedLength = mapped_length;
  mapping->bytes = (const uint8_t *)base + leading_bytes;
  return YES;
}

- (void)unmapOriginalMapping:(OmniMarkdownFileMapping *)mapping {
  // Return only this lease's actual mapping and clear all exposed pointers, including empty leases.
  if (mapping->base != NULL) {
    (void)madvise(mapping->base, mapping->mappedLength, MADV_DONTNEED);
    munmap(mapping->base, mapping->mappedLength);
  }
  mapping->base = NULL;
  mapping->mappedLength = 0;
  mapping->bytes = NULL;
}

- (BOOL)bytesForPiece:(OmniMarkdownPiece)piece
                  bytes:(const uint8_t **)bytes
                mapping:(OmniMarkdownFileMapping *)mapping
                  error:(NSError **)error {
  // Original pieces acquire a scoped map; session additions borrow this document's append-only storage.
  mapping->base = NULL;
  mapping->mappedLength = 0;
  mapping->bytes = NULL;
  if (piece.source == OmniMarkdownPieceSourceOriginalFile) {
    NSRange source_range = NSMakeRange(piece.sourceOffset, piece.byteLength);
    if (![self mapOriginalRange:source_range mapping:mapping error:error]) {
      return NO;
    }
    *bytes = mapping->bytes;
    return YES;
  }
  if (piece.sourceOffset > _sessionAdditionBytes.length ||
      piece.byteLength > _sessionAdditionBytes.length - piece.sourceOffset) {
    if (error != NULL) {
      *error = omni_markdown_model_error(11, @"The session edit range is invalid.");
    }
    return NO;
  }
  *bytes = (const uint8_t *)_sessionAdditionBytes.bytes + piece.sourceOffset;
  return YES;
}

- (BOOL)appendSliceOfPiece:(OmniMarkdownPiece)piece
          localUTF16Range:(NSRange)localRange
                  storage:(NSMutableData *)storage
                    error:(NSError **)error {
  // Derive a metadata-only subpiece after checking both scalar boundaries in one live source lease.
  const uint8_t *bytes = NULL;
  OmniMarkdownFileMapping mapping = {0};
  if (![self bytesForPiece:piece bytes:&bytes mapping:&mapping error:error]) {
    return NO;
  }
  NSUInteger start_byte = 0;
  NSUInteger end_byte = 0;
  BOOL offsets_valid =
      localRange.location <= piece.utf16Length &&
      localRange.length <= piece.utf16Length - localRange.location &&
      markdown_byte_offset_for_utf16(bytes, piece.byteLength,
                                      localRange.location, &start_byte) &&
      markdown_byte_offset_for_utf16(bytes, piece.byteLength,
                                      NSMaxRange(localRange), &end_byte);
  if (!offsets_valid) {
    [self unmapOriginalMapping:&mapping];
    if (error != NULL) {
      *error = omni_markdown_model_error(
          12, @"The edit does not align with a Unicode character boundary.");
    }
    return NO;
  }
  OmniMarkdownTextMetrics metrics = {0, 0};
  BOOL metrics_valid = measure_markdown_utf8(bytes + start_byte,
                                             end_byte - start_byte, &metrics);
  // Metadata no longer needs source bytes; release the lease before error propagation or piece publication.
  [self unmapOriginalMapping:&mapping];
  if (!metrics_valid) {
    if (error != NULL) {
      *error = omni_markdown_model_error(13, @"The edit slice is not valid UTF-8.");
    }
    return NO;
  }
  OmniMarkdownPiece sliced_piece = {
      .source = piece.source,
      .sourceOffset = piece.sourceOffset + start_byte,
      .byteLength = end_byte - start_byte,
      .utf16Length = metrics.utf16Length,
      .newlineCount = metrics.newlineCount,
  };
  append_markdown_piece(storage, sliced_piece);
  return YES;
}

- (BOOL)replaceUTF16Range:(NSRange)range
                  withText:(NSString *)text
                     error:(NSError **)error {
  // Prepare replacement metadata off to the side; the original piece sequence stays authoritative on failure.
  if (range.location > _utf16Length ||
      range.length > _utf16Length - range.location) {
    if (error != NULL) {
      *error = omni_markdown_model_error(14, @"The requested edit range is outside the document.");
    }
    return NO;
  }
  NSData *inserted_utf8 = [text dataUsingEncoding:NSUTF8StringEncoding
                              allowLossyConversion:NO];
  // Only inserted text is encoded; original document bytes remain descriptor-backed piece spans.
  if (inserted_utf8 == nil) {
    if (error != NULL) {
      *error = omni_markdown_model_error(15, @"The inserted text is not valid Unicode.");
    }
    return NO;
  }
  OmniMarkdownTextMetrics inserted_metrics = {0, 0};
  if (!measure_markdown_utf8(inserted_utf8.bytes, inserted_utf8.length,
                             &inserted_metrics)) {
    if (error != NULL) {
      *error = omni_markdown_model_error(16, @"The inserted text is not valid UTF-8.");
    }
    return NO;
  }

  NSUInteger addition_rollback_length = _sessionAdditionBytes.length;
  // Failed prefix/suffix slicing rolls the append-only edit buffer back to this exact prior length.
  OmniMarkdownPiece inserted_piece = {
      .source = OmniMarkdownPieceSourceSessionAddition,
      .sourceOffset = addition_rollback_length,
      .byteLength = inserted_utf8.length,
      .utf16Length = inserted_metrics.utf16Length,
      .newlineCount = inserted_metrics.newlineCount,
  };
  if (inserted_utf8.length > 0) {
    [_sessionAdditionBytes appendData:inserted_utf8];
  }

  NSData *old_piece_snapshot = [_pieceStorage copy];
  const OmniMarkdownPiece *old_pieces = old_piece_snapshot.bytes;
  NSUInteger old_piece_count =
      old_piece_snapshot.length / sizeof(OmniMarkdownPiece);
  NSMutableData *new_piece_storage = [[NSMutableData alloc] init];
  NSUInteger replacement_end = NSMaxRange(range);
  NSUInteger global_utf16 = 0;
  BOOL inserted = NO;

  // Preserve nonoverlapping old pieces, split only the intersected boundaries, and insert new text once.
  for (NSUInteger index = 0; index < old_piece_count; index += 1) {
    OmniMarkdownPiece piece = old_pieces[index];
    NSUInteger piece_start = global_utf16;
    NSUInteger piece_end = piece_start + piece.utf16Length;
    if (piece_end <= range.location) {
      append_markdown_piece(new_piece_storage, piece);
    } else if (piece_start >= replacement_end) {
      if (!inserted) {
        append_markdown_piece(new_piece_storage, inserted_piece);
        inserted = YES;
      }
      append_markdown_piece(new_piece_storage, piece);
    } else {
      if (range.location > piece_start) {
        NSRange prefix =
            NSMakeRange(0, range.location - piece_start);
        if (![self appendSliceOfPiece:piece
                      localUTF16Range:prefix
                              storage:new_piece_storage
                                error:error]) {
          [_sessionAdditionBytes setLength:addition_rollback_length];
          return NO;
        }
      }
      if (!inserted) {
        append_markdown_piece(new_piece_storage, inserted_piece);
        inserted = YES;
      }
      if (replacement_end < piece_end) {
        NSRange suffix = NSMakeRange(replacement_end - piece_start,
                                    piece_end - replacement_end);
        if (![self appendSliceOfPiece:piece
                      localUTF16Range:suffix
                              storage:new_piece_storage
                                error:error]) {
          [_sessionAdditionBytes setLength:addition_rollback_length];
          return NO;
        }
      }
    }
    global_utf16 = piece_end;
  }
  if (!inserted) {
    append_markdown_piece(new_piece_storage, inserted_piece);
  }

  _pieceStorage = new_piece_storage;
  // Publish the completed replacement only after every borrowed slice has been validated and unmapped.
  _utf16Length = _utf16Length - range.length + inserted_metrics.utf16Length;
  NSUInteger newline_count = 0;
  const OmniMarkdownPiece *new_pieces = _pieceStorage.bytes;
  NSUInteger new_piece_count = _pieceStorage.length / sizeof(OmniMarkdownPiece);
  for (NSUInteger index = 0; index < new_piece_count; index += 1) {
    newline_count += new_pieces[index].newlineCount;
  }
  _lineCount = newline_count + 1;
  return YES;
}

- (BOOL)appendPiece:(OmniMarkdownPiece)piece
    localUTF16Range:(NSRange)localRange
          byteBudget:(NSUInteger)byteBudget
              toData:(NSMutableData *)data
     appendedUTF16Length:(NSUInteger *)appendedUTF16Length
               error:(NSError **)error {
  // Copy only scalar-complete bytes within the remaining caller budget, returning the actual UTF-16 extent.
  const uint8_t *piece_bytes = NULL;
  OmniMarkdownFileMapping mapping = {0};
  if (![self bytesForPiece:piece
                      bytes:&piece_bytes
                    mapping:&mapping
                      error:error]) {
    return NO;
  }
  NSUInteger start_byte = 0;
  NSUInteger end_byte = 0;
  BOOL offsets_valid =
      markdown_byte_offset_for_utf16(piece_bytes, piece.byteLength,
                                      localRange.location, &start_byte) &&
      markdown_byte_offset_for_utf16(piece_bytes, piece.byteLength,
                                      NSMaxRange(localRange), &end_byte);
  if (!offsets_valid) {
    [self unmapOriginalMapping:&mapping];
    if (error != NULL) {
      *error = omni_markdown_model_error(
          17, @"The requested text range is not on a Unicode boundary.");
    }
    return NO;
  }

  const uint8_t *slice_bytes = piece_bytes + start_byte;
  NSUInteger slice_length = end_byte - start_byte;
  NSUInteger accepted_bytes = 0;
  NSUInteger accepted_utf16 = 0;
  while (accepted_bytes < slice_length && accepted_bytes < byteBudget) {
    // A scalar exceeding the last budget bytes ends the prefix without splitting its encoding.
    NSUInteger scalar_byte_length = 0;
    NSUInteger scalar_utf16_length = 0;
    if (!decode_markdown_utf8_scalar(slice_bytes, slice_length, accepted_bytes,
                                     &scalar_byte_length,
                                     &scalar_utf16_length)) {
      [self unmapOriginalMapping:&mapping];
      if (error != NULL) {
        *error = omni_markdown_model_error(18, @"The viewport contains invalid UTF-8.");
      }
      return NO;
    }
    if (scalar_byte_length > byteBudget - accepted_bytes) {
      break;
    }
    accepted_bytes += scalar_byte_length;
    accepted_utf16 += scalar_utf16_length;
  }
  if (accepted_bytes > 0) {
    [data appendBytes:slice_bytes length:accepted_bytes];
  }
  [self unmapOriginalMapping:&mapping];
  *appendedUTF16Length = accepted_utf16;
  return YES;
}

- (nullable NSString *)stringForUTF16Range:(NSRange)range
                                byteBudget:(NSUInteger)byteBudget
                         actualUTF16Length:(NSUInteger *)actualUTF16Length
                                     error:(NSError **)error {
  // The returned string is budget-limited even when the requested logical range spans the document.
  if (range.location > _utf16Length ||
      range.length > _utf16Length - range.location) {
    if (error != NULL) {
      *error = omni_markdown_model_error(19, @"The requested display range is outside the document.");
    }
    return nil;
  }
  NSMutableData *utf8 = [[NSMutableData alloc]
      initWithCapacity:MIN(byteBudget, range.length * 2 + 16)];
  const OmniMarkdownPiece *pieces = _pieceStorage.bytes;
  NSUInteger piece_count = _pieceStorage.length / sizeof(OmniMarkdownPiece);
  NSUInteger global_utf16 = 0;
  NSUInteger appended_utf16 = 0;
  for (NSUInteger index = 0;
       index < piece_count && appended_utf16 < range.length &&
       utf8.length < byteBudget;
       index += 1) {
    // Skip nonintersecting metadata and debit one shared byte ceiling across every contributing piece.
    OmniMarkdownPiece piece = pieces[index];
    NSUInteger piece_start = global_utf16;
    NSUInteger piece_end = piece_start + piece.utf16Length;
    NSUInteger requested_end = NSMaxRange(range);
    if (piece_end > range.location && piece_start < requested_end) {
      NSUInteger local_start =
          MAX(piece_start, range.location) - piece_start;
      NSUInteger local_end = MIN(piece_end, requested_end) - piece_start;
      NSUInteger piece_utf16_appended = 0;
      if (![self appendPiece:piece
             localUTF16Range:NSMakeRange(local_start, local_end - local_start)
                   byteBudget:byteBudget - utf8.length
                       toData:utf8
          appendedUTF16Length:&piece_utf16_appended
                        error:error]) {
        return nil;
      }
      appended_utf16 += piece_utf16_appended;
      if (piece_utf16_appended < local_end - local_start) {
        break;
      }
    }
    global_utf16 = piece_end;
  }
  NSString *string = [[NSString alloc] initWithData:utf8
                                           encoding:NSUTF8StringEncoding];
  if (string == nil) {
    if (error != NULL) {
      *error = omni_markdown_model_error(20, @"The displayed text could not be decoded.");
    }
    return nil;
  }
  if (actualUTF16Length != NULL) {
    *actualUTF16Length = appended_utf16;
  }
  return string;
}

- (NSUInteger)utf16OffsetForLine:(NSUInteger)line {
  // Skip pieces by newline counts, then inspect only the piece containing the requested LF boundary.
  if (line == 0) {
    return 0;
  }
  if (line >= _lineCount) {
    return _utf16Length;
  }
  const OmniMarkdownPiece *pieces = _pieceStorage.bytes;
  NSUInteger piece_count = _pieceStorage.length / sizeof(OmniMarkdownPiece);
  NSUInteger remaining_newlines = line;
  NSUInteger global_utf16 = 0;
  for (NSUInteger index = 0; index < piece_count; index += 1) {
    OmniMarkdownPiece piece = pieces[index];
    if (remaining_newlines > piece.newlineCount) {
      remaining_newlines -= piece.newlineCount;
      global_utf16 += piece.utf16Length;
      continue;
    }
    if (remaining_newlines == 0) {
      return global_utf16;
    }
    const uint8_t *piece_bytes = NULL;
    OmniMarkdownFileMapping mapping = {0};
    if (![self bytesForPiece:piece
                        bytes:&piece_bytes
                      mapping:&mapping
                        error:NULL]) {
      return _utf16Length;
    }
    NSUInteger byte_offset = 0;
    NSUInteger local_utf16 = 0;
    while (byte_offset < piece.byteLength) {
      NSUInteger scalar_byte_length = 0;
      NSUInteger scalar_utf16_length = 0;
      if (!decode_markdown_utf8_scalar(piece_bytes, piece.byteLength,
                                       byte_offset, &scalar_byte_length,
                                       &scalar_utf16_length)) {
        [self unmapOriginalMapping:&mapping];
        return _utf16Length;
      }
      BOOL is_newline = piece_bytes[byte_offset] == '\n';
      byte_offset += scalar_byte_length;
      local_utf16 += scalar_utf16_length;
      if (is_newline) {
        remaining_newlines -= 1;
        if (remaining_newlines == 0) {
          [self unmapOriginalMapping:&mapping];
          return global_utf16 + local_utf16;
        }
      }
    }
    [self unmapOriginalMapping:&mapping];
    global_utf16 += piece.utf16Length;
  }
  return _utf16Length;
}

- (void)getLine:(NSUInteger *)line
      utf16Column:(NSUInteger *)column
    forUTF16Offset:(NSUInteger)offset {
  // Locate the bounded logical position with metadata plus one piece scan, then derive its UTF-16 column.
  NSUInteger bounded_offset = MIN(offset, _utf16Length);
  const OmniMarkdownPiece *pieces = _pieceStorage.bytes;
  NSUInteger piece_count = _pieceStorage.length / sizeof(OmniMarkdownPiece);
  NSUInteger global_utf16 = 0;
  NSUInteger line_number = 0;
  for (NSUInteger index = 0; index < piece_count; index += 1) {
    OmniMarkdownPiece piece = pieces[index];
    if (global_utf16 + piece.utf16Length < bounded_offset) {
      line_number += piece.newlineCount;
      global_utf16 += piece.utf16Length;
      continue;
    }
    NSUInteger local_limit = bounded_offset - global_utf16;
    const uint8_t *piece_bytes = NULL;
    OmniMarkdownFileMapping mapping = {0};
    if ([self bytesForPiece:piece
                      bytes:&piece_bytes
                    mapping:&mapping
                      error:NULL]) {
      NSUInteger byte_limit = 0;
      if (markdown_byte_offset_for_utf16(piece_bytes, piece.byteLength,
                                          local_limit, &byte_limit)) {
        for (NSUInteger byte_offset = 0; byte_offset < byte_limit;) {
          NSUInteger scalar_byte_length = 0;
          NSUInteger scalar_utf16_length = 0;
          if (!decode_markdown_utf8_scalar(piece_bytes, byte_limit, byte_offset,
                                           &scalar_byte_length,
                                           &scalar_utf16_length)) {
            break;
          }
          if (piece_bytes[byte_offset] == '\n') {
            line_number += 1;
          }
          byte_offset += scalar_byte_length;
        }
      }
      [self unmapOriginalMapping:&mapping];
    }
    break;
  }
  NSUInteger line_start = [self utf16OffsetForLine:line_number];
  if (line != NULL) {
    *line = line_number;
  }
  if (column != NULL) {
    *column = bounded_offset >= line_start ? bounded_offset - line_start : 0;
  }
}

@end

static int markdown_model_self_test_failure(NSString *reason) {
  // This standalone failure status concerns the comparison model, never full native product acceptance.
  fprintf(stderr, "Markdown model self-test failed: %s\n", reason.UTF8String);
  return 1;
}

static int run_markdown_model_self_tests(const char *fixture_path,
                                         const char *executable_path) {
  // Use a caller-owned tiny synthetic Unicode file to exercise actual mappings and piece edits.
  NSError *error = nil;
  OmniMappedMarkdownDocument *document = [[OmniMappedMarkdownDocument alloc]
      initWithFileSystemRepresentation:fixture_path
                              error:&error];
  if (document == nil) {
    return markdown_model_self_test_failure(error.localizedDescription);
  }
  if (document.utf16Length != 13 || document.lineCount != 4) {
    return markdown_model_self_test_failure(
        @"Initial UTF-16 length or line count is incorrect.");
  }

  NSUInteger actual_utf16_length = 0;
  NSString *initial_text = [document
      stringForUTF16Range:NSMakeRange(0, document.utf16Length)
               byteBudget:1024
        actualUTF16Length:&actual_utf16_length
                    error:&error];
  if (initial_text == nil || actual_utf16_length != 13 ||
      ![initial_text isEqualToString:@"A😀B\n中文\nlast\n"]) {
    return markdown_model_self_test_failure(
        @"Initial fixture did not round-trip through the mapped model.");
  }

  error = nil;
  if ([document replaceUTF16Range:NSMakeRange(2, 0)
                         withText:@"x"
                            error:&error] ||
      error.code != 12) {
    // The requested insertion lands inside the fixture's astral scalar and must remain rejected.
    return markdown_model_self_test_failure(
        @"An edit split a UTF-16 surrogate pair.");
  }

  error = nil;
  NSString *invalid_boundary_text = [document
      stringForUTF16Range:NSMakeRange(2, 1)
               byteBudget:1024
        actualUTF16Length:&actual_utf16_length
                    error:&error];
  if (invalid_boundary_text != nil || error.code != 17) {
    return markdown_model_self_test_failure(
        @"A display range split a UTF-16 surrogate pair.");
  }

  error = nil;
  NSString *budgeted_text = [document
      stringForUTF16Range:NSMakeRange(0, document.utf16Length)
               byteBudget:2
        actualUTF16Length:&actual_utf16_length
                    error:&error];
  // A two-byte ceiling may return the ASCII prefix but cannot include part of the following emoji.
  if (budgeted_text == nil || ![budgeted_text isEqualToString:@"A"] ||
      actual_utf16_length != 1) {
    return markdown_model_self_test_failure(
        @"The viewport byte budget split a UTF-8 scalar.");
  }

  error = nil;
  if (![document replaceUTF16Range:NSMakeRange(3, 0)
                          withText:@"-"
                             error:&error] ||
      ![document replaceUTF16Range:NSMakeRange(4, 3)
                          withText:@""
                             error:&error] ||
      ![document replaceUTF16Range:NSMakeRange(document.utf16Length, 0)
                          withText:@"尾\n"
                             error:&error]) {
    // These actual edits cross original/addition pieces without replacing the source fixture on disk.
    return markdown_model_self_test_failure(error.localizedDescription);
  }

  error = nil;
  NSString *edited_text = [document
      stringForUTF16Range:NSMakeRange(0, document.utf16Length)
               byteBudget:1024
        actualUTF16Length:&actual_utf16_length
                    error:&error];
  if (edited_text == nil || document.utf16Length != 13 ||
      document.lineCount != 4 || actual_utf16_length != 13 ||
      ![edited_text isEqualToString:@"A😀-文\nlast\n尾\n"]) {
    return markdown_model_self_test_failure(
        @"Piece insertion, deletion, or split produced the wrong document.");
  }
  if ([document utf16OffsetForLine:1] != 6 ||
      [document utf16OffsetForLine:2] != 11 ||
      [document utf16OffsetForLine:3] != 13) {
    return markdown_model_self_test_failure(
        @"Line-to-UTF-16 offsets are incorrect after edits.");
  }
  NSUInteger line = NSNotFound;
  NSUInteger column = NSNotFound;
  [document getLine:&line utf16Column:&column forUTF16Offset:12];
  if (line != 2 || column != 1) {
    return markdown_model_self_test_failure(
        @"UTF-16-to-line offset is incorrect after edits.");
  }
  [document getLine:&line utf16Column:&column forUTF16Offset:13];
  if (line != 3 || column != 0) {
    return markdown_model_self_test_failure(
        @"End-of-document line offset is incorrect.");
  }

  error = nil;
  if ([document replaceUTF16Range:NSMakeRange(document.utf16Length + 1, 0)
                         withText:@"x"
                            error:&error] ||
      error.code != 14) {
    return markdown_model_self_test_failure(
        @"An edit outside the document was accepted.");
  }

  int fixture_descriptor = open(fixture_path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
  // Reconcile one real descriptor with its source path and reject the unrelated executable path.
  struct stat fixture_status;
  if (fixture_descriptor < 0 ||
      fstat(fixture_descriptor, &fixture_status) != 0) {
    if (fixture_descriptor >= 0) {
      close(fixture_descriptor);
    }
    return markdown_model_self_test_failure(
        @"The revision fixture could not be inspected.");
  }
  BOOL matching_identity = markdown_file_revision_matches(
      fixture_descriptor, fixture_path, (NSUInteger)fixture_status.st_size,
      fixture_status.st_dev, fixture_status.st_ino,
      fixture_status.st_mtimespec);
  BOOL replaced_path_rejected = !markdown_file_revision_matches(
      fixture_descriptor, executable_path,
      (NSUInteger)fixture_status.st_size, fixture_status.st_dev,
      fixture_status.st_ino, fixture_status.st_mtimespec);
  close(fixture_descriptor);
  if (!matching_identity || !replaced_path_rejected) {
    return markdown_model_self_test_failure(
        @"The open descriptor and current path revision were not reconciled.");
  }

  printf("markdown_model_self_tests=pass\n");
  return 0;
}

@interface OmniMappedMarkdownViewportView : NSView <NSTextInputClient>
@property(nonatomic, strong) OmniMappedMarkdownDocument *document;
@property(nonatomic) NSRange selectedTextRange;
@property(nonatomic) NSRange markedTextRange;
@property(nonatomic) NSUInteger firstVisibleLine;
@property(nonatomic, copy)
    NSDictionary<NSAttributedStringKey, id> *textDrawingAttributes;
@property(nonatomic) CGFloat lineHeight;
@property(nonatomic) CGFloat characterWidth;
@end

@implementation OmniMappedMarkdownViewportView

- (instancetype)initWithFrame:(NSRect)frame
                      document:(OmniMappedMarkdownDocument *)document {
  // Own this comparison's document and finite drawing attributes; no hidden TextKit editor is constructed.
  self = [super initWithFrame:frame];
  if (self != nil) {
    _document = document;
    _selectedTextRange = NSMakeRange(0, 0);
    _markedTextRange = NSMakeRange(NSNotFound, 0);
    NSFont *font = [NSFont monospacedSystemFontOfSize:15.0
                                              weight:NSFontWeightRegular];
    _textDrawingAttributes = @{
      NSFontAttributeName : font,
      NSForegroundColorAttributeName : NSColor.labelColor,
    };
    _lineHeight = ceil(font.ascender - font.descender + font.leading + 5.0);
    _characterWidth = ceil([@"M" sizeWithAttributes:_textDrawingAttributes].width);
    [self refreshDocumentGeometry];
  }
  return self;
}

- (BOOL)isFlipped {
  // Use top-origin rows consistently for this control's drawing, hit tests and approximate caret.
  return YES;
}

- (BOOL)isOpaque {
  // The surrounding native window supplies its background; the comparison does not cover every dirty pixel.
  return NO;
}

- (BOOL)acceptsFirstResponder {
  // AppKit may deliver real text-input callbacks only after this concrete view takes focus.
  return YES;
}

- (NSUInteger)visibleLineCapacity {
  // Retain at least one logical row for a short window; this is fixed-height comparison geometry.
  CGFloat available_height = MAX(1.0, self.bounds.size.height - 32.0);
  return MAX((NSUInteger)1,
             (NSUInteger)floor(available_height / self.lineHeight));
}

- (void)refreshDocumentGeometry {
  // Clamp scrolling after an edit shrinks the line count, without materializing document text.
  NSUInteger visible_line_capacity = [self visibleLineCapacity];
  NSUInteger maximum_first_line =
      self.document.lineCount > visible_line_capacity
          ? self.document.lineCount - visible_line_capacity
          : 0;
  self.firstVisibleLine = MIN(self.firstVisibleLine, maximum_first_line);
}

- (NSRange)boundedReplacementRange:(NSRange)requestedRange {
  // An unspecified native range targets composition or selection; stale extents clamp in this old control.
  NSRange candidate = requestedRange;
  if (candidate.location == NSNotFound) {
    candidate = self.markedTextRange.location == NSNotFound
                    ? self.selectedTextRange
                    : self.markedTextRange;
  }
  if (candidate.location > self.document.utf16Length) {
    candidate = NSMakeRange(self.document.utf16Length, 0);
  } else if (candidate.length >
             self.document.utf16Length - candidate.location) {
    candidate.length = self.document.utf16Length - candidate.location;
  }
  return candidate;
}

- (NSString *)plainTextFromInput:(id)input {
  // Accept only public plain/attributed string payloads, discarding styling from this plain-text comparison.
  if ([input isKindOfClass:NSAttributedString.class]) {
    return ((NSAttributedString *)input).string;
  }
  if ([input isKindOfClass:NSString.class]) {
    return (NSString *)input;
  }
  return @"";
}

- (void)refreshAfterEdit {
  // Keep the current selection's line in view, then invalidate only the comparison caret's coordinates.
  [self refreshDocumentGeometry];
  self.needsDisplay = YES;
  NSUInteger line = 0;
  NSUInteger column = 0;
  [self.document getLine:&line
             utf16Column:&column
           forUTF16Offset:self.selectedTextRange.location];
  NSUInteger visible_line_capacity = [self visibleLineCapacity];
  if (line < self.firstVisibleLine) {
    self.firstVisibleLine = line;
  } else if (line >= self.firstVisibleLine + visible_line_capacity) {
    self.firstVisibleLine = line - visible_line_capacity + 1;
  }
  NSRect caret = NSMakeRect(24.0 + (CGFloat)column * self.characterWidth,
                            16.0 +
                                (CGFloat)(line - self.firstVisibleLine) *
                                    self.lineHeight,
                            2.0,
                            self.lineHeight);
  [self setNeedsDisplayInRect:NSInsetRect(caret, -24.0, -self.lineHeight)];
  [self.inputContext invalidateCharacterCoordinates];
}

- (void)insertText:(id)input replacementRange:(NSRange)replacementRange {
  // Commit through the actual comparison document first; failure preserves selection and composition.
  NSString *inserted_text = [self plainTextFromInput:input];
  NSRange bounded_range = [self boundedReplacementRange:replacementRange];
  if (![self.document replaceUTF16Range:bounded_range
                               withText:inserted_text
                                  error:NULL]) {
    NSBeep();
    return;
  }
  self.selectedTextRange =
      NSMakeRange(bounded_range.location + inserted_text.length, 0);
  self.markedTextRange = NSMakeRange(NSNotFound, 0);
  if (inserted_text.length > 0 &&
      ![inserted_text canBeConvertedToEncoding:NSASCIIStringEncoding]) {
    self.window.title = @"Omni Markdown IME committed";
  }
  [self refreshAfterEdit];
}

- (void)setMarkedText:(id)input
         selectedRange:(NSRange)selectedRange
       replacementRange:(NSRange)replacementRange {
  // Provisional text replaces the declared target; the input method's relative selection stays inside it.
  NSString *marked_text = [self plainTextFromInput:input];
  NSRange bounded_range = [self boundedReplacementRange:replacementRange];
  if (![self.document replaceUTF16Range:bounded_range
                               withText:marked_text
                                  error:NULL]) {
    NSBeep();
    return;
  }
  self.markedTextRange = NSMakeRange(bounded_range.location, marked_text.length);
  NSUInteger selected_location =
      bounded_range.location + MIN(selectedRange.location, marked_text.length);
  NSUInteger selected_length =
      MIN(selectedRange.length,
          bounded_range.location + marked_text.length - selected_location);
  self.selectedTextRange = NSMakeRange(selected_location, selected_length);
  [self refreshAfterEdit];
}

- (void)unmarkText {
  // End provisional marking without deleting the text already represented by document pieces.
  self.markedTextRange = NSMakeRange(NSNotFound, 0);
  self.needsDisplay = YES;
}

- (BOOL)hasMarkedText {
  // NSNotFound identifies no composition independently from whether its current length is zero.
  return self.markedTextRange.location != NSNotFound;
}

- (NSRange)selectedRange {
  // Return edited-document UTF-16 coordinates, never an original backing-file byte range.
  return self.selectedTextRange;
}

- (NSRange)markedRange {
  // Return the comparison's exact provisional range for the public NSTextInputClient protocol.
  return self.markedTextRange;
}

- (nullable NSAttributedString *)attributedSubstringForProposedRange:(NSRange)range
                                                          actualRange:(NSRangePointer)actualRange {
  // Input context reads share a fixed byte ceiling; explicitly report any shorter returned UTF-16 prefix.
  NSRange bounded_range = [self boundedReplacementRange:range];
  NSUInteger actual_length = 0;
  NSString *text = [self.document
      stringForUTF16Range:bounded_range
               byteBudget:kMaximumInputContextUTF8Bytes
        actualUTF16Length:&actual_length
                    error:NULL];
  if (text == nil) {
    return nil;
  }
  if (actualRange != NULL) {
    *actualRange = NSMakeRange(bounded_range.location, actual_length);
  }
  return [[NSAttributedString alloc] initWithString:text
                                         attributes:self.textDrawingAttributes];
}

- (NSArray<NSAttributedStringKey> *)validAttributesForMarkedText {
  // The old direct-drawing control implements text composition without attributed marked-text styling.
  return @[];
}

- (NSRect)firstRectForCharacterRange:(NSRange)range
                          actualRange:(NSRangePointer)actualRange {
  // This approximate monospaced anchor is a comparison limitation. It cannot certify real
  // variable-width Chinese shaping, ligatures, bidi navigation or product candidate placement.
  NSUInteger bounded_location = MIN(range.location, self.document.utf16Length);
  NSUInteger line = 0;
  NSUInteger column = 0;
  [self.document getLine:&line
             utf16Column:&column
           forUTF16Offset:bounded_location];
  NSUInteger visible_line_capacity = [self visibleLineCapacity];
  NSUInteger displayed_line =
      line <= self.firstVisibleLine
          ? 0
          : MIN(line - self.firstVisibleLine, visible_line_capacity - 1);
  NSRect local_rect = NSMakeRect(24.0 + (CGFloat)column * self.characterWidth,
                                 16.0 + (CGFloat)displayed_line * self.lineHeight,
                                 2.0,
                                 self.lineHeight);
  NSRect window_rect = [self convertRect:local_rect toView:nil];
  if (actualRange != NULL) {
    *actualRange = NSMakeRange(bounded_location, 0);
  }
  return [self.window convertRectToScreen:window_rect];
}

- (NSUInteger)characterIndexForPoint:(NSPoint)point {
  // Translate screen coordinates into this control's fixed row/column grid and clamp to its logical extent.
  NSPoint window_point = [self.window convertPointFromScreen:point];
  NSPoint local_point = [self convertPoint:window_point fromView:nil];
  NSUInteger line =
      self.firstVisibleLine +
      (local_point.y <= 16.0
           ? 0
           : (NSUInteger)((local_point.y - 16.0) / self.lineHeight));
  line = MIN(line, self.document.lineCount - 1);
  NSUInteger line_start = [self.document utf16OffsetForLine:line];
  NSUInteger line_end = [self.document utf16OffsetForLine:line + 1];
  NSUInteger requested_column =
      local_point.x <= 24.0
          ? 0
          : (NSUInteger)llround((local_point.x - 24.0) / self.characterWidth);
  return line_start + MIN(requested_column, line_end - line_start);
}

- (void)mouseDown:(NSEvent *)event {
  // Acquire real focus before replacing this comparison's selection with the approximate hit-test result.
  [self.window makeFirstResponder:self];
  NSPoint point = [self convertPoint:event.locationInWindow fromView:nil];
  NSPoint screen_point = [self.window convertPointToScreen:event.locationInWindow];
  (void)point;
  NSUInteger offset = [self characterIndexForPoint:screen_point];
  self.selectedTextRange = NSMakeRange(offset, 0);
  self.markedTextRange = NSMakeRange(NSNotFound, 0);
  [self refreshAfterEdit];
}

- (void)keyDown:(NSEvent *)event {
  // Only a physical Escape exits this standalone comparison. IME cancel remains a protocol command.
  if (event.keyCode == 53) {
    [NSApp terminate:nil];
    return;
  }
  [self interpretKeyEvents:@[ event ]];
}

- (void)scrollWheel:(NSEvent *)event {
  // Scroll logical rows within the existing finite document and invalidate the current native input anchor.
  CGFloat delta = event.scrollingDeltaY;
  if (fabs(delta) < 0.01) {
    return;
  }
  NSUInteger line_delta =
      MAX((NSUInteger)1, (NSUInteger)ceil(fabs(delta) / self.lineHeight));
  NSUInteger visible_line_capacity = [self visibleLineCapacity];
  NSUInteger maximum_first_line =
      self.document.lineCount > visible_line_capacity
          ? self.document.lineCount - visible_line_capacity
          : 0;
  if (delta > 0.0) {
    self.firstVisibleLine =
        line_delta >= self.firstVisibleLine
            ? 0
            : self.firstVisibleLine - line_delta;
  } else {
    self.firstVisibleLine =
        MIN(maximum_first_line, self.firstVisibleLine + line_delta);
  }
  self.needsDisplay = YES;
  [self.inputContext invalidateCharacterCoordinates];
}

- (NSRange)previousComposedCharacterRange {
  // This old comparison inspects at most sixteen preceding UTF-16 units. It is not an
  // unbounded-grapheme correctness guarantee or the product's streaming boundary algorithm.
  if (self.selectedTextRange.location == 0) {
    return NSMakeRange(NSNotFound, 0);
  }
  NSUInteger window_start =
      self.selectedTextRange.location > 16
          ? self.selectedTextRange.location - 16
          : 0;
  NSRange prefix_range = NSMakeRange(
      window_start, self.selectedTextRange.location - window_start);
  NSUInteger actual_length = 0;
  NSString *prefix = [self.document
      stringForUTF16Range:prefix_range
               byteBudget:kMaximumInputContextUTF8Bytes
        actualUTF16Length:&actual_length
                    error:NULL];
  if (prefix.length == 0 || actual_length != prefix_range.length) {
    return NSMakeRange(NSNotFound, 0);
  }
  NSRange composed =
      [prefix rangeOfComposedCharacterSequenceAtIndex:prefix.length - 1];
  return NSMakeRange(window_start + composed.location, composed.length);
}

- (NSRange)nextComposedCharacterRange {
  // Symmetric finite lookahead belongs only to this comparison; no document-sized string is returned.
  if (self.selectedTextRange.location >= self.document.utf16Length) {
    return NSMakeRange(NSNotFound, 0);
  }
  NSUInteger remaining =
      self.document.utf16Length - self.selectedTextRange.location;
  NSRange suffix_range =
      NSMakeRange(self.selectedTextRange.location, MIN((NSUInteger)16, remaining));
  NSUInteger actual_length = 0;
  NSString *suffix = [self.document
      stringForUTF16Range:suffix_range
               byteBudget:kMaximumInputContextUTF8Bytes
        actualUTF16Length:&actual_length
                    error:NULL];
  if (suffix.length == 0 || actual_length == 0) {
    return NSMakeRange(NSNotFound, 0);
  }
  NSRange composed = [suffix rangeOfComposedCharacterSequenceAtIndex:0];
  return NSMakeRange(self.selectedTextRange.location, composed.length);
}

- (void)doCommandBySelector:(SEL)selector {
  // This finite comparison command set directly edits its piece model. Product history,
  // link actions and richer navigation remain owned exclusively by the production plugin.
  if (selector == @selector(deleteBackward:)) {
    NSRange deletion = self.selectedTextRange.length > 0
                           ? self.selectedTextRange
                           : [self previousComposedCharacterRange];
    if (deletion.location != NSNotFound &&
        [self.document replaceUTF16Range:deletion withText:@"" error:NULL]) {
      self.selectedTextRange = NSMakeRange(deletion.location, 0);
      self.markedTextRange = NSMakeRange(NSNotFound, 0);
      [self refreshAfterEdit];
    }
  } else if (selector == @selector(deleteForward:)) {
    NSRange deletion = self.selectedTextRange.length > 0
                           ? self.selectedTextRange
                           : [self nextComposedCharacterRange];
    if (deletion.location != NSNotFound &&
        [self.document replaceUTF16Range:deletion withText:@"" error:NULL]) {
      self.selectedTextRange = NSMakeRange(deletion.location, 0);
      self.markedTextRange = NSMakeRange(NSNotFound, 0);
      [self refreshAfterEdit];
    }
  } else if (selector == @selector(insertNewline:)) {
    [self insertText:@"\n" replacementRange:NSMakeRange(NSNotFound, 0)];
  } else if (selector == @selector(insertTab:)) {
    [self insertText:@"\t" replacementRange:NSMakeRange(NSNotFound, 0)];
  } else if (selector == @selector(moveLeft:)) {
    NSRange previous = [self previousComposedCharacterRange];
    if (previous.location != NSNotFound) {
      self.selectedTextRange = NSMakeRange(previous.location, 0);
      [self refreshAfterEdit];
    }
  } else if (selector == @selector(moveRight:)) {
    NSRange next = [self nextComposedCharacterRange];
    if (next.location != NSNotFound) {
      self.selectedTextRange = NSMakeRange(NSMaxRange(next), 0);
      [self refreshAfterEdit];
    }
  } else if (selector == @selector(moveToBeginningOfDocument:)) {
    self.selectedTextRange = NSMakeRange(0, 0);
    [self refreshAfterEdit];
  } else if (selector == @selector(moveToEndOfDocument:)) {
    self.selectedTextRange = NSMakeRange(self.document.utf16Length, 0);
    [self refreshAfterEdit];
  } else if (selector == @selector(selectAll:)) {
    // Selection is a logical range only; selecting the whole source does not read or copy its text.
    self.selectedTextRange = NSMakeRange(0, self.document.utf16Length);
    self.needsDisplay = YES;
  } else if (selector == @selector(cancelOperation:)) {
    // Input methods own this command. Real Escape is handled in keyDown:.
  }
}

- (void)drawRect:(NSRect)dirtyRect {
  (void)dirtyRect;
  // Request the visible line span under a separate UTF-8 ceiling, then split only that bounded string.
  NSUInteger first_line = self.firstVisibleLine;
  NSUInteger visible_line_count = [self visibleLineCapacity] + 1;
  NSUInteger last_line =
      MIN(self.document.lineCount, first_line + visible_line_count);
  NSUInteger start_offset = [self.document utf16OffsetForLine:first_line];
  NSUInteger end_offset = [self.document utf16OffsetForLine:last_line];
  NSUInteger actual_length = 0;
  NSString *visible_text = [self.document
      stringForUTF16Range:NSMakeRange(start_offset, end_offset - start_offset)
               byteBudget:kMaximumViewportUTF8Bytes
        actualUTF16Length:&actual_length
                    error:NULL];
  if (visible_text == nil) {
    // A changed original revision paints an explicit comparison failure rather than stale document text.
    [@"The document changed outside this editor session."
        drawAtPoint:NSMakePoint(24.0, 16.0 + (CGFloat)first_line * self.lineHeight)
     withAttributes:self.textDrawingAttributes];
    return;
  }

  NSArray<NSString *> *lines = [visible_text componentsSeparatedByString:@"\n"];
  NSUInteger maximum_drawn_lines = MIN(lines.count, last_line - first_line + 1);
  for (NSUInteger index = 0; index < maximum_drawn_lines; index += 1) {
    [lines[index]
        drawAtPoint:NSMakePoint(24.0,
                                16.0 + (CGFloat)index * self.lineHeight)
     withAttributes:self.textDrawingAttributes];
  }

  if (self.selectedTextRange.length == 0) {
    // Paint only a collapsed comparison caret; selection highlighting is intentionally absent here.
    NSUInteger caret_line = 0;
    NSUInteger caret_column = 0;
    [self.document getLine:&caret_line
               utf16Column:&caret_column
             forUTF16Offset:self.selectedTextRange.location];
    if (caret_line >= first_line && caret_line <= last_line) {
      [NSColor.controlAccentColor setFill];
      NSRectFill(NSMakeRect(24.0 + (CGFloat)caret_column * self.characterWidth,
                            16.0 +
                                (CGFloat)(caret_line - self.firstVisibleLine) *
                                    self.lineHeight,
                            1.5,
                            self.lineHeight));
    }
  }
  (void)actual_length;
}

@end

@interface OmniMappedMarkdownWindow : NSWindow
@end

@implementation OmniMappedMarkdownWindow

- (BOOL)canBecomeKeyWindow {
  // A borderless standalone comparison still needs actual text-input focus.
  return YES;
}

- (BOOL)canBecomeMainWindow {
  // Permit normal activation without creating title-bar controls.
  return YES;
}

@end

@interface OmniMappedMarkdownApplicationDelegate
    : NSObject <NSApplicationDelegate>
@property(nonatomic, strong) NSWindow *markdownWindow;
@property(nonatomic, strong) OmniMappedMarkdownDocument *document;
@property(nonatomic, strong) OmniMappedMarkdownViewportView *editorView;
@end

@implementation OmniMappedMarkdownApplicationDelegate

- (void)applicationDidFinishLaunching:(NSNotification *)notification {
  (void)notification;
  // The delegate retains this one comparison window and its actual view/document graph.
  const NSRect window_frame = NSMakeRect(0.0, 0.0, 1180.0, 760.0);
  self.markdownWindow = [[OmniMappedMarkdownWindow alloc]
      initWithContentRect:window_frame
                styleMask:NSWindowStyleMaskBorderless
                  backing:NSBackingStoreBuffered
                    defer:NO];
  self.markdownWindow.title = @"Omni Markdown Native Floor";
  self.markdownWindow.releasedWhenClosed = NO;
  self.markdownWindow.backgroundColor = NSColor.textBackgroundColor;

  self.editorView = [[OmniMappedMarkdownViewportView alloc]
      initWithFrame:NSMakeRect(0.0, 0.0, window_frame.size.width,
                               window_frame.size.height)
            document:self.document];
  self.editorView.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
  // Publish the completed content graph before requesting native focus or obtaining its input context.
  self.markdownWindow.contentView = self.editorView;
  [self.markdownWindow center];
  [self.markdownWindow makeKeyAndOrderFront:nil];
  [self.markdownWindow makeFirstResponder:self.editorView];
  (void)self.editorView.inputContext;
  [NSApp activateIgnoringOtherApps:YES];
}

- (BOOL)applicationShouldTerminateAfterLastWindowClosed:
    (NSApplication *)sender {
  (void)sender;
  // Candidate-window teardown is not a request to exit this resident standalone comparison.
  return NO;
}

@end

int main(int argc, const char *argv[]) {
  @autoreleasepool {
    // The explicit tiny-fixture mode runs without a GUI; ordinary mode owns its dedicated AppKit loop.
    if (argc == 3 && strcmp(argv[1], "--self-test") == 0) {
      return run_markdown_model_self_tests(argv[2], argv[0]);
    }
    if (argc != 2) {
      fprintf(stderr,
              "usage: %s <utf8-markdown-file> | --self-test <fixture>\n",
              argv[0]);
      return 64;
    }
    NSError *document_error = nil;
    // Open only the caller's explicit path; this executable never resolves the production OMNI directory.
    OmniMappedMarkdownDocument *document = [[OmniMappedMarkdownDocument alloc]
        initWithFileSystemRepresentation:argv[1]
                                error:&document_error];
    if (document == nil) {
      fprintf(stderr, "cannot open Markdown benchmark document: %s\n",
              document_error.localizedDescription.UTF8String);
      return 1;
    }

    NSApplication *application = NSApplication.sharedApplication;
    application.activationPolicy = NSApplicationActivationPolicyAccessory;
    OmniMappedMarkdownApplicationDelegate *delegate =
        [[OmniMappedMarkdownApplicationDelegate alloc] init];
    delegate.document = document;
    application.delegate = delegate;
    [application run];
  }
  return 0;
}
