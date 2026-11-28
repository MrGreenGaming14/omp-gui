/* C ABI of the loa biscuit-capi fork (cpp/libs/biscuit_ffi/fork/src/loa.rs).
 * Status-returning calls report details through the thread's last error;
 * pointer-returning calls return NULL on error. Free every handle with its
 * `_free` function (NULL accepted). */
#ifndef LOA_BISCUIT_FFI_LOA_BISCUIT_H
#define LOA_BISCUIT_FFI_LOA_BISCUIT_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum LoaBscStatus {
  LOA_BSC_OK = 0,
  LOA_BSC_INVALID_ARGUMENT = 1,
  LOA_BSC_INTERNAL = 2,
  LOA_BSC_FORMAT = 3,
  LOA_BSC_FORMAT_SIGNATURE = 4,
  LOA_BSC_FORMAT_UNKNOWN_PUBLIC_KEY = 5,
  LOA_BSC_APPEND_ON_SEALED = 6,
  LOA_BSC_ALREADY_SEALED = 7,
  LOA_BSC_NO_MATCHING_POLICY = 8,
  LOA_BSC_UNAUTHORIZED = 9,
  LOA_BSC_INVALID_BLOCK_RULE = 10,
  LOA_BSC_AUTHORIZER_NOT_EMPTY = 11,
  LOA_BSC_LANGUAGE = 12,
  LOA_BSC_TOO_MANY_FACTS = 13,
  LOA_BSC_TOO_MANY_ITERATIONS = 14,
  LOA_BSC_TIMEOUT = 15,
  LOA_BSC_UNEXPECTED_QUERY_RESULT = 16,
  LOA_BSC_CONVERSION = 17,
  LOA_BSC_BASE64 = 18,
  LOA_BSC_EXECUTION = 19,
} LoaBscStatus;

typedef enum LoaBscTermKind {
  LOA_BSC_TERM_OTHER = 0,
  LOA_BSC_TERM_STRING = 1,
  LOA_BSC_TERM_INTEGER = 2,
  LOA_BSC_TERM_SET = 3,
} LoaBscTermKind;

/* Rust-owned buffer; release with loa_bsc_bytes_free. */
typedef struct LoaBscBytes {
  uint8_t* data;
  size_t len;
} LoaBscBytes;

typedef struct LoaBscKeyPair LoaBscKeyPair;
typedef struct LoaBscPublicKey LoaBscPublicKey;
typedef struct LoaBscTerm LoaBscTerm;
typedef struct LoaBscParams LoaBscParams;
typedef struct LoaBscBiscuitBuilder LoaBscBiscuitBuilder;
typedef struct LoaBscBlockBuilder LoaBscBlockBuilder;
typedef struct LoaBscBiscuit LoaBscBiscuit;
typedef struct LoaBscThirdPartyRequest LoaBscThirdPartyRequest;
typedef struct LoaBscThirdPartyBlock LoaBscThirdPartyBlock;
typedef struct LoaBscAuthorizerBuilder LoaBscAuthorizerBuilder;
typedef struct LoaBscAuthorizer LoaBscAuthorizer;
typedef struct LoaBscFacts LoaBscFacts;

/* errors */
LoaBscStatus loa_bsc_error_status(void);
const char* loa_bsc_error_message(void);
size_t loa_bsc_error_failed_check_count(void);
bool loa_bsc_error_failed_check(size_t index, bool* is_block, uint32_t* block_id, uint32_t* check_id, const char** rule);

void loa_bsc_bytes_free(LoaBscBytes bytes);

/* keys (Ed25519) */
LoaBscKeyPair* loa_bsc_key_pair_from_seed(const uint8_t* seed, size_t len);
LoaBscPublicKey* loa_bsc_key_pair_public(const LoaBscKeyPair* kp);
LoaBscStatus loa_bsc_key_pair_sign(const LoaBscKeyPair* kp, const uint8_t* data, size_t len, LoaBscBytes* out);
void loa_bsc_key_pair_free(LoaBscKeyPair* kp);
LoaBscPublicKey* loa_bsc_public_key_from_bytes(const uint8_t* data, size_t len);
size_t loa_bsc_public_key_bytes(const LoaBscPublicKey* key, uint8_t* out32);
void loa_bsc_public_key_free(LoaBscPublicKey* key);

/* terms and parameters */
LoaBscTerm* loa_bsc_term_string(const char* s);
LoaBscTerm* loa_bsc_term_int(int64_t v);
LoaBscTerm* loa_bsc_term_set(void);
LoaBscStatus loa_bsc_term_set_insert(LoaBscTerm* set, const LoaBscTerm* element);
LoaBscTerm* loa_bsc_term_clone(const LoaBscTerm* term);
LoaBscTermKind loa_bsc_term_kind(const LoaBscTerm* term);
bool loa_bsc_term_as_string(const LoaBscTerm* term, const char** data, size_t* len);
bool loa_bsc_term_as_int(const LoaBscTerm* term, int64_t* out);
size_t loa_bsc_term_set_len(const LoaBscTerm* term);
LoaBscTerm* loa_bsc_term_set_at(const LoaBscTerm* term, size_t index);
void loa_bsc_term_free(LoaBscTerm* term);

LoaBscParams* loa_bsc_params_new(void);
LoaBscStatus loa_bsc_params_insert(LoaBscParams* params, const char* name, const LoaBscTerm* term);
void loa_bsc_params_free(LoaBscParams* params);

/* authority block */
LoaBscBiscuitBuilder* loa_bsc_biscuit_builder_new(void);
LoaBscStatus loa_bsc_biscuit_builder_fact(LoaBscBiscuitBuilder* b, const char* name, const LoaBscTerm* const* terms, size_t count);
LoaBscStatus loa_bsc_biscuit_builder_code(LoaBscBiscuitBuilder* b, const char* source, const LoaBscParams* params);
LoaBscStatus loa_bsc_biscuit_builder_context(LoaBscBiscuitBuilder* b, const char* context);
LoaBscBiscuit* loa_bsc_biscuit_builder_build(const LoaBscBiscuitBuilder* b, const LoaBscKeyPair* root);
void loa_bsc_biscuit_builder_free(LoaBscBiscuitBuilder* b);

/* attenuation block */
LoaBscBlockBuilder* loa_bsc_block_builder_new(void);
LoaBscStatus loa_bsc_block_builder_fact(LoaBscBlockBuilder* b, const char* name, const LoaBscTerm* const* terms, size_t count);
LoaBscStatus loa_bsc_block_builder_code(LoaBscBlockBuilder* b, const char* source, const LoaBscParams* params);
LoaBscStatus loa_bsc_block_builder_context(LoaBscBlockBuilder* b, const char* context);
void loa_bsc_block_builder_free(LoaBscBlockBuilder* b);

/* token */
LoaBscBiscuit* loa_bsc_biscuit_from(const uint8_t* data, size_t len, const LoaBscPublicKey* root);
LoaBscStatus loa_bsc_biscuit_to_vec(const LoaBscBiscuit* b, LoaBscBytes* out);
size_t loa_bsc_biscuit_block_count(const LoaBscBiscuit* b);
LoaBscStatus loa_bsc_biscuit_block_context(const LoaBscBiscuit* b, size_t index, LoaBscBytes* out);
LoaBscPublicKey* loa_bsc_biscuit_block_external_key(const LoaBscBiscuit* b, size_t index);
void loa_bsc_biscuit_free(LoaBscBiscuit* b);

/* third party */
LoaBscThirdPartyRequest* loa_bsc_biscuit_third_party_request(const LoaBscBiscuit* b);
/* Consumes `request`. */
LoaBscThirdPartyBlock* loa_bsc_third_party_request_create_block(LoaBscThirdPartyRequest* request, const LoaBscKeyPair* signer, const LoaBscBlockBuilder* block);
void loa_bsc_third_party_request_free(LoaBscThirdPartyRequest* request);
void loa_bsc_third_party_block_free(LoaBscThirdPartyBlock* block);
LoaBscBiscuit* loa_bsc_biscuit_append_third_party(const LoaBscBiscuit* b, const LoaBscPublicKey* signer, const LoaBscThirdPartyBlock* block);

/* authorizer */
LoaBscAuthorizerBuilder* loa_bsc_authorizer_builder_new(void);
LoaBscStatus loa_bsc_authorizer_builder_code(LoaBscAuthorizerBuilder* b, const char* source, const LoaBscParams* params);
LoaBscStatus loa_bsc_authorizer_builder_fact(LoaBscAuthorizerBuilder* b, const char* name, const LoaBscTerm* const* terms, size_t count);
LoaBscStatus loa_bsc_authorizer_builder_set_limits(LoaBscAuthorizerBuilder* b, uint64_t max_facts, uint64_t max_iterations, uint64_t max_time_us);
LoaBscAuthorizer* loa_bsc_authorizer_builder_build(const LoaBscAuthorizerBuilder* b, const LoaBscBiscuit* token);
void loa_bsc_authorizer_builder_free(LoaBscAuthorizerBuilder* b);
LoaBscStatus loa_bsc_authorizer_authorize(LoaBscAuthorizer* a, size_t* policy);
LoaBscFacts* loa_bsc_authorizer_query(LoaBscAuthorizer* a, const char* rule);
void loa_bsc_authorizer_free(LoaBscAuthorizer* a);

size_t loa_bsc_facts_len(const LoaBscFacts* f);
bool loa_bsc_facts_name(const LoaBscFacts* f, size_t index, const char** data, size_t* len);
size_t loa_bsc_facts_term_count(const LoaBscFacts* f, size_t index);
const LoaBscTerm* loa_bsc_facts_term(const LoaBscFacts* f, size_t index, size_t term);
void loa_bsc_facts_free(LoaBscFacts* f);

#ifdef __cplusplus
}
#endif

#endif
