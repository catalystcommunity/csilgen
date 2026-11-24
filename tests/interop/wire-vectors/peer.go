// Decodes every vector as the shape its kind names; an ok row must also re-encode to
// its canonical bytes.
package main

import (
	"bufio"
	"encoding/hex"
	"fmt"
	"os"
	"strings"

	api "wirecheck/api"
)

// roundTrip decodes data as the kind and encodes the value again.
func roundTrip(kind string, data []byte) ([]byte, error, error) {
	switch kind {
	case "holder":
		v, err := api.DecodeHolder(data)
		if err != nil {
			return nil, err, nil
		}
		out, err := api.EncodeHolder(v)
		return out, nil, err
	case "contact":
		v, err := api.DecodeContact(data)
		if err != nil {
			return nil, err, nil
		}
		out, err := api.EncodeContact(v)
		return out, nil, err
	case "output":
		v, err := api.DecodeLookupReadCountsOutput(data)
		if err != nil {
			return nil, err, nil
		}
		out, err := api.EncodeLookupReadCountsOutput(v)
		return out, nil, err
	default:
		return nil, fmt.Errorf("unknown kind %s", kind), nil
	}
}

func check(kind, expect, input, canonical string) string {
	data, err := hex.DecodeString(input)
	if err != nil {
		return "bad input hex"
	}
	encoded, decErr, encErr := roundTrip(kind, data)
	switch {
	case expect == "err" && decErr == nil:
		return "decoded, but must fail"
	case expect == "err":
		return ""
	case expect != "ok":
		return "unknown expectation " + expect
	case decErr != nil:
		return "refused: " + decErr.Error()
	case encErr != nil:
		return "encode refused: " + encErr.Error()
	}
	if again := hex.EncodeToString(encoded); again != canonical {
		return "encoded " + again + ", want " + canonical
	}
	return ""
}

func main() {
	file, err := os.Open(os.Args[1])
	if err != nil {
		panic(err)
	}
	defer file.Close()
	failures := 0
	lines := bufio.NewScanner(file)
	for lines.Scan() {
		line := lines.Text()
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		cols := strings.Split(line, "\t")
		if len(cols) != 5 {
			panic("malformed row " + line)
		}
		if problem := check(cols[1], cols[2], cols[3], cols[4]); problem != "" {
			failures++
			fmt.Printf("FAIL %s: %s\n", cols[0], problem)
		}
	}
	if err := lines.Err(); err != nil {
		panic(err)
	}
	if failures > 0 {
		fmt.Printf("%d vector(s) failed\n", failures)
		os.Exit(1)
	}
	fmt.Println("all wire vectors pass")
}
