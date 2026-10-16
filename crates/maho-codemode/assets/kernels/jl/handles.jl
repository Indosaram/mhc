# Lazy handle host helpers are loaded only when `handle()` is first used.
const SENPI_HANDLE_KINDS = ("agent", "completion", "workpool")
const SENPI_WAIT_MODES = ("all", "any", "settled")
const SENPI_HANDLE_USAGE = "handle() expects an agent(...; handle=true) record, a workpool, a completion handle, or a saved {kind, id, run_epoch} reference"

struct SenpiHandleControl
    ref::Dict{String, Any}
end

struct SenpiHandle <: AbstractDict{String, Any}
    fields::Dict{String, Any}
    ref::Dict{String, Any}
end

Base.iterate(value::SenpiHandle, state...) = iterate(getfield(value, :fields), state...)
Base.length(value::SenpiHandle) = length(getfield(value, :fields))
Base.get(value::SenpiHandle, key, default) = get(getfield(value, :fields), key, default)
Base.getindex(value::SenpiHandle, key) = getfield(value, :fields)[key]
Base.haskey(value::SenpiHandle, key) = haskey(getfield(value, :fields), key)
Base.keys(value::SenpiHandle) = keys(getfield(value, :fields))
Base.values(value::SenpiHandle) = values(getfield(value, :fields))

function Base.getproperty(value::SenpiHandle, name::Symbol)
    name === :ref && return copy(getfield(value, :ref))
    name === :control && return SenpiHandleControl(copy(getfield(value, :ref)))
    fields = getfield(value, :fields)
    key = string(name)
    haskey(fields, key) && return fields[key]
    getfield(value, name)
end

Base.propertynames(value::SenpiHandle) = (Symbol.(collect(keys(getfield(value, :fields))))..., :control, :ref)

function senpi_handle_ref(value)
    value isa SenpiHandle && return copy(getfield(value, :ref))
    value isa SenpiWorkpool && return Dict{String, Any}("kind" => "workpool", "id" => getfield(value, :pool_id), "run_epoch" => 0)
    value isa AbstractDict || error(SENPI_HANDLE_USAGE)
    record = Dict{String, Any}(string(key) => item for (key, item) in value)
    pool_id = get(record, "pool_id", nothing)
    pool_id isa AbstractString && return Dict{String, Any}("kind" => "workpool", "id" => string(pool_id), "run_epoch" => 0)
    kind = get(record, "kind", nothing)
    if !(kind in SENPI_HANDLE_KINDS)
        uri = get(record, "handle", nothing)
        scheme = uri isa AbstractString ? first(split(uri, "://"; limit=2)) : nothing
        kind = scheme in SENPI_HANDLE_KINDS ? scheme : nothing
    end
    identity = get(record, "id", get(record, "task_id", nothing))
    (kind === nothing || !(identity isa AbstractString) || isempty(identity)) && error(SENPI_HANDLE_USAGE)
    run_epoch = get(record, "run_epoch", kind == "workpool" ? 0 : nothing)
    (run_epoch isa Integer && !(run_epoch isa Bool) && run_epoch >= 0) || error(SENPI_HANDLE_USAGE * "; run_epoch must be a non-negative integer")
    Dict{String, Any}("kind" => string(kind), "id" => string(identity), "run_epoch" => Int(run_epoch))
end

function senpi_handle_call(tool_name::String, arguments)
    senpi_with_bridge_timeout_pause(() -> senpi_call_tool(tool_name, arguments))
end

function senpi_wait(handles; timeout=nothing, mode="all")
    items = handles === nothing ? Any[] : handles isa AbstractVector || handles isa Tuple ? collect(handles) : Any[handles]
    if timeout !== nothing
        (timeout isa Real && !(timeout isa Bool) && isfinite(timeout) && timeout >= 0) || error("wait() timeout must be a finite number of seconds >= 0")
    end
    string(mode) in SENPI_WAIT_MODES || error("wait() mode must be \"all\", \"any\" or \"settled\"")
    arguments = Dict{String, Any}("refs" => [senpi_handle_ref(item) for item in items], "mode" => string(mode))
    timeout === nothing || (arguments["timeout"] = timeout)
    senpi_handle_call("__wait__", arguments)
end

function Base.getproperty(control::SenpiHandleControl, name::Symbol)
    ref = getfield(control, :ref)
    name === :ref && return copy(ref)
    name === :status && return () -> senpi_handle_call("__handle_status__", Dict("ref" => ref))
    name === :output && return (; format="raw", offset=nothing, limit=nothing) -> begin
        arguments = Dict{String, Any}("ref" => ref, "format" => string(format))
        offset === nothing || (arguments["offset"] = offset)
        limit === nothing || (arguments["limit"] = limit)
        senpi_handle_call("__handle_output__", arguments)
    end
    name === :send && return message -> senpi_handle_call("__handle_send__", Dict("ref" => ref, "message" => string(message)))
    name === :cancel && return () -> senpi_handle_call("__handle_cancel__", Dict("ref" => ref))
    name === :wait && return (; timeout=nothing) -> senpi_wait([ref]; timeout=timeout, mode="all")[1]
    getfield(control, name)
end

Base.propertynames(::SenpiHandleControl) = (:status, :output, :send, :cancel, :wait, :ref)

function senpi_handle_view(value)
    ref = senpi_handle_ref(value)
    fields = Dict{String, Any}()
    if value isa AbstractDict
        for (key, item) in value
            item isa Function || (fields[string(key)] = item)
        end
    end
    get!(fields, "id", ref["id"])
    get!(fields, "run_epoch", ref["run_epoch"])
    get!(fields, "handle", ref["kind"] * "://" * ref["id"])
    SenpiHandle(fields, ref)
end

Base.wait(value::SenpiHandle; timeout=nothing, mode="all") = senpi_wait([value]; timeout=timeout, mode=mode)
Base.wait(values::AbstractVector{<:SenpiHandle}; timeout=nothing, mode="all") = senpi_wait(values; timeout=timeout, mode=mode)
