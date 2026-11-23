alias Csilgen.Generated.{Cbor, Req, Resp, RpClient, RpError, RpErrorRefusal}
alias Csilgen.Generated.RpErrorCode

42 = RpErrorCode.to_number(:account_rate_limited)
:account_rate_limited = RpErrorCode.from_number(42)

defmodule Handler do
  @behaviour Csilgen.Generated.RpServer
  def decrypt_token(%Req{a: "limit"}, _ctx),
    do: {:error, %RpError{code: :application_user_limit, message: "limit"}}

  def decrypt_token(_req, _ctx), do: {:ok, %Resp{b: "ok"}}
end

# Answers like a server: the variant travels beside the payload, and the request
# text can force a missing or unknown variant.
defmodule Carrier do
  @behaviour Csilgen.Generated.Transport
  defstruct []

  def call(_carrier, _service, _op, _bytes),
    do: raise("a multi-arm operation must use call_reply")

  def call_reply(_carrier, "Rp", "decrypt-token", bytes) do
    request = Req.from_cbor(bytes)
    if request.a == "unknown", do: raise(RuntimeError, "status 2")
    {variant, payload} = Cbor.encode_rp_decrypt_token_reply(Handler.decrypt_token(request, %{}))
    if variant == "RpError", do: 7 = :binary.at(payload, 6)

    case request.a do
      "other" -> {"Other", payload}
      "none" -> {nil, payload}
      _ -> {variant, payload}
    end
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

for forced <- ["other", "none"] do
  try do
    RpClient.decrypt_token(client, %Req{a: forced})
    raise "accepted variant #{forced}"
  rescue
    _ in ArgumentError -> :ok
  end
end

try do
  RpClient.decrypt_token(client, %Req{a: "unknown"})
  raise "missing transport error"
rescue
  e in RuntimeError -> "status 2" = e.message
end

IO.puts("elixir: typed replies pass")
