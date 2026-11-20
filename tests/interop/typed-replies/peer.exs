alias Csilgen.Generated.{Cbor, Req, Resp, RpClient, RpError, RpErrorRefusal}

defmodule Handler do
  @behaviour Csilgen.Generated.RpServer
  def decrypt_token(%Req{a: "limit"}, _ctx),
    do: {:error, %RpError{code: :application_user_limit, message: "limit"}}

  def decrypt_token(_req, _ctx), do: {:ok, %Resp{b: "ok"}}
end

defmodule Carrier do
  @behaviour Csilgen.Generated.Transport
  defstruct []
  def call(_carrier, service, op, bytes) do
    request = Req.from_cbor(bytes)
    if request.a == "unknown", do: raise(RuntimeError, "status 2")
    {variant, payload} = Cbor.encode_rp_decrypt_token_reply(Handler.decrypt_token(request, %{}))
    if variant == "RpError", do: 7 = :binary.at(payload, 6)
    Cbor.resolve_rpc_reply(service, op, variant, payload)
  end
end

client = RpClient.new(struct(Carrier))
%Resp{b: "ok"} = RpClient.decrypt_token(client, %Req{a: "ok"})

try do
  RpClient.decrypt_token(client, %Req{a: "limit"})
  raise "missing refusal"
rescue
  e in RpErrorRefusal -> %RpError{code: :application_user_limit} = e.value
end

try do
  RpClient.decrypt_token(client, %Req{a: "unknown"})
  raise "missing transport error"
rescue
  e in RuntimeError -> "status 2" = e.message
end
