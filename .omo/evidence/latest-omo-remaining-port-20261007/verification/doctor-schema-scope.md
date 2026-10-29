# Doctor schema controlling scope

The pinned OMO checkout at /home/indo/code/oh-my-openagent is 77f3067f157a4f88e6d8ed48b3a6c338654402ed. Its script/build-help-schemas.ts:3 imports DoctorResultSchema exclusively from packages/omo-opencode/src/help/schema/doctor; lines 37-43 declare the doctor schema output. The same script imports status, sandbox, and ACP help schemas from the OpenCode adapter. The doctor schema is not a native senpi producer contract.

Therefore inv:schema:2 is governed by scope-opencode-excluded, not an in-scope missing native schema. Its former claim that assets/help/doctor.schema.json exists in the native tree is false. The row must be explicit_exclusion / closed_scope with this producer citation and the controlling user decision, rather than implementing an unwanted OpenCode compatibility command. This finding corrects the worker's absence-based ambiguity using actual pinned producer evidence.
