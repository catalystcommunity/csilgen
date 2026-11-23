// Decodes every vector as a Stamp; an ok row must also re-encode to its canonical bytes.
package main

import (
	"bufio"
	"encoding/hex"
	"fmt"
	"os"
	"strings"

	api "stampcheck/api"
)

func check(expect, input, canonical string) string {
	data, err := hex.DecodeString(input)
	if err != nil {
		return "bad input hex"
	}
	stamp, err := api.DecodeStamp(data)
	switch {
	case expect == "err" && err == nil:
		return "accepted " + stamp.At.String()
	case expect == "err":
		return ""
	case expect != "ok":
		return "unknown expectation " + expect
	case err != nil:
		return "refused: " + err.Error()
	}
	encoded, err := api.EncodeStamp(stamp)
	if err != nil {
		return "encode refused: " + err.Error()
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
	rows, failures := 0, 0
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
		rows++
		if problem := check(cols[1], cols[3], cols[4]); problem != "" {
			failures++
			fmt.Printf("FAIL %s: %s\n", cols[0], problem)
		}
	}
	if err := lines.Err(); err != nil {
		panic(err)
	}
	fmt.Printf("go: %d of %d timestamp vectors pass\n", rows-failures, rows)
	if failures > 0 {
		os.Exit(1)
	}
}
