require_relative "client"

raise unless RpErrorCode.to_number(RpErrorCode::ACCOUNT_RATE_LIMITED) == 42
raise unless RpErrorCode.from_number(42) == RpErrorCode::ACCOUNT_RATE_LIMITED

class Carrier
  def call(service, op, payload)
    req = Req.from_cbor(payload)
    raise IOError, "2" if req.a == "unknown"
    variant, reply = CsilCbor.encode_rp_decrypt_token_reply do
      if req.a == "limit"
        raise CsilCbor::RpErrorRefusal.new(RpError.new(code: RpErrorCode::APPLICATION_USER_LIMIT, message: "limit"))
      end
      Resp.new(b: "ok")
    end
    raise "wrong CBOR error code" if variant == "RpError" && reply.getbyte(6) != 7
    CsilCbor.resolve_rpc_reply(service, op, variant, reply)
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
