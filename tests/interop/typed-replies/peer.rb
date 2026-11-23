require_relative "client"

raise unless RpErrorCode.to_number(RpErrorCode::ACCOUNT_RATE_LIMITED) == 42
raise unless RpErrorCode.from_number(42) == RpErrorCode::ACCOUNT_RATE_LIMITED

# A fake transport: the generated client must pick the reply arm from the variant
# this seam returns, with no help from the resolver.
class Carrier
  def call_reply(_service, _op, payload)
    req = Req.from_cbor(payload)
    raise IOError, "2" if req.a == "unknown"
    return ["Other", Resp.new(b: "other").to_cbor] if req.a == "other"
    return [nil, Resp.new(b: "none").to_cbor] if req.a == "none"

    variant, reply = CsilCbor.encode_rp_decrypt_token_reply do
      if req.a == "limit"
        raise CsilCbor::RpErrorRefusal.new(RpError.new(code: RpErrorCode::APPLICATION_USER_LIMIT, message: "limit"))
      end
      Resp.new(b: "ok")
    end
    raise "wrong CBOR error code" if variant == "RpError" && reply.getbyte(6) != 7
    [variant, reply]
  end
end

client = RpClient.new(Carrier.new)
raise unless client.decrypt_token(Req.new(a: "ok")).b == "ok"
begin
  client.decrypt_token(Req.new(a: "limit"))
  raise "missing refusal"
rescue CsilCbor::RpErrorRefusal => e
  raise unless e.value.code.equal?(RpErrorCode::APPLICATION_USER_LIMIT)
end
begin
  client.decrypt_token(Req.new(a: "unknown"))
  raise "missing transport error"
rescue IOError => e
  raise unless e.message == "2"
end
%w[other none].each do |bad|
  client.decrypt_token(Req.new(a: bad))
  raise "accepted reply with #{bad} variant"
rescue ArgumentError
  # An unknown or missing variant is a decode error.
end

# A transport with only the bytes-only `call` seam cannot serve a multi-arm op.
class BytesOnly
  def call(_service, _op, _payload) = Resp.new(b: "ok").to_cbor
end
begin
  RpClient.new(BytesOnly.new).decrypt_token(Req.new(a: "ok"))
  raise "missing NotImplementedError"
rescue NotImplementedError
  # expected
end
puts "typed replies pass"
