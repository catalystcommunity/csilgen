alias Csilgen.Generated.{Contact, Holder, LookupReadCountsOutput}

# Decode each vector as the type its kind names; re-encode the rows that must decode.
[path] = System.argv()
modules = %{"holder" => Holder, "contact" => Contact, "output" => LookupReadCountsOutput}

failures =
  path
  |> File.read!()
  |> String.split("\n", trim: true)
  |> Enum.reject(&String.starts_with?(&1, "#"))
  |> Enum.flat_map(fn line ->
    [name, kind, expect, input, canonical] = String.split(line, "\t")
    module = Map.fetch!(modules, kind)

    decoded =
      try do
        {:ok, module.from_cbor(Base.decode16!(input, case: :lower))}
      rescue
        e -> {:error, e}
      end

    case {expect, decoded} do
      {"ok", {:ok, value}} ->
        got = Base.encode16(module.to_cbor(value), case: :lower)
        if got == canonical, do: [], else: ["FAIL #{name}: re-encoded #{got}, want #{canonical}"]

      {"ok", {:error, e}} ->
        ["FAIL #{name}: decode failed: #{Exception.message(e)}"]

      {"err", {:ok, value}} ->
        ["FAIL #{name}: decoded, but must fail: #{inspect(value)}"]

      {"err", {:error, _}} ->
        []
    end
  end)

Enum.each(failures, &IO.puts/1)
if failures != [], do: System.halt(1)
IO.puts("all wire vectors pass")
