#!/bin/sh
printf '%s\n' "$@" > "$ARGS_FILE"
rid() { printf '%s' "$1" | sed 's/.*"request_id":"\([^"]*\)".*/\1/'; }
ok() { printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$1"; }
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG_FILE"
  case "$line" in
    *'"type":"control_response"'*)
      case "$line" in
        *'"mcp_response"'*)
          printf '%s\n' '{"type":"user","parent_tool_use_id":null,"session_id":"cc-fake","uuid":"u-2","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"file-a\nfile-b","is_error":false}]}}'
          printf '%s\n' '{"type":"assistant","parent_tool_use_id":null,"session_id":"cc-fake","uuid":"a-2","message":{"id":"msg_2","type":"message","role":"assistant","model":"fake-model","content":[{"type":"text","text":"Done."}]}}'
          printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"duration_ms":5,"num_turns":1,"session_id":"cc-fake","total_cost_usd":0.01,"usage":{"input_tokens":11,"output_tokens":7}}'
          ;;
        *'"behavior":"allow"'*)
          printf '%s\n' '{"type":"control_request","request_id":"mcp-1","request":{"subtype":"mcp_message","server_name":"loa-host","message":{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"project_search","arguments":{"query":"auth"}}}}}'
          ;;
      esac
      ;;
    *'"type":"control_request"'*)
      case "$line" in
        *'"subtype":"initialize"'*)
          printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"commands":[],"models":[{"value":"default","displayName":"Default (recommended)","description":"Opus"},{"value":"haiku","displayName":"Haiku","description":"Fast","supportedEffortLevels":["low","medium","high"]}]}}}\n' "$(rid "$line")"
          ;;
        *'"subtype":"set_model"'*)
          case "$line" in
            *'"model":"bad-model"'*)
              printf '{"type":"control_response","response":{"subtype":"error","request_id":"%s","error":"unknown model"}}\n' "$(rid "$line")"
              ;;
            *) ok "$(rid "$line")" ;;
          esac
          ;;
        *'"subtype":"interrupt"'*)
          ok "$(rid "$line")"
          printf '%s\n' '{"type":"result","subtype":"error_during_execution","is_error":true,"terminal_reason":"aborted_streaming","session_id":"cc-fake"}'
          ;;
        *) ok "$(rid "$line")" ;;
      esac
      ;;
    *'"type":"user"'*)
      case "$line" in
        *'use the tool'*)
          printf '%s\n' '{"type":"system","subtype":"init","session_id":"cc-fake","model":"fake-model","permissionMode":"default","tools":["Bash"]}'
          printf '%s\n' 'warning: not json'
          printf '%s\n' '{"type":"stream_event","parent_tool_use_id":null,"session_id":"cc-fake","uuid":"e1","event":{"type":"message_start","message":{"id":"msg_1","model":"fake-model"}}}'
          printf '%s\n' '{"type":"stream_event","parent_tool_use_id":null,"session_id":"cc-fake","uuid":"e2","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}'
          printf '%s\n' '{"type":"stream_event","parent_tool_use_id":null,"session_id":"cc-fake","uuid":"e3","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Listing."}}}'
          printf '%s\n' '{"type":"stream_event","parent_tool_use_id":null,"session_id":"cc-fake","uuid":"e4","event":{"type":"content_block_stop","index":0}}'
          printf '%s\n' '{"type":"assistant","parent_tool_use_id":null,"session_id":"cc-fake","uuid":"a-1","message":{"id":"msg_1","type":"message","role":"assistant","model":"fake-model","content":[{"type":"text","text":"Listing."}]}}'
          printf '%s\n' '{"type":"assistant","parent_tool_use_id":null,"session_id":"cc-fake","uuid":"a-1b","message":{"id":"msg_1","type":"message","role":"assistant","model":"fake-model","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}]}}'
          printf '%s\n' '{"type":"control_request","request_id":"perm-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"toolu_1"}}'
          ;;
        *'hang'*)
          printf '%s\n' '{"type":"system","subtype":"init","session_id":"cc-fake","model":"fake-model","permissionMode":"default","tools":[]}'
          ;;
        *'die'*)
          echo "fatal: lost the API connection" >&2
          exit 3
          ;;
      esac
      ;;
  esac
done
