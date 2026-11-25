// toml-go PARSER FILE COUNT: parse FILE COUNT times into a generic map; PARSER is
// pelletier, burntsushi, or none (read only); ast is go-toml's own parser alone
// (unstable.Parser), each expression read and no Go values made: the closest to what Witchy does.
package main

import (
	"fmt"
	"os"
	"strconv"

	burnt "github.com/BurntSushi/toml"
	pelletier "github.com/pelletier/go-toml/v2"
	"github.com/pelletier/go-toml/v2/unstable"
)

var sink any

func main() {
	data, err := os.ReadFile(os.Args[2])
	if err != nil {
		panic(err)
	}
	count, _ := strconv.Atoi(os.Args[3])
	total := 0
	for i := 0; i < count; i++ {
		switch os.Args[1] {
		case "pelletier":
			var v map[string]any
			if pelletier.Unmarshal(data, &v) == nil {
				total++
			}
			sink = v
		case "ast":
			var p unstable.Parser
			p.Reset(data)
			n := 0
			for p.NextExpression() {
				n++
			}
			if p.Error() == nil {
				total += n
			}
		case "burntsushi":
			var v map[string]any
			if _, err := burnt.Decode(string(data), &v); err == nil {
				total++
			}
			sink = v
		default:
			total += len(data)
		}
	}
	fmt.Println(total)
}
