package main
import("fmt";"time")
func main(){const n=__MESSAGES__;t0:=time.Now();valid:=true;for i:=0;i<n;i++{ch:=make(chan string,1);ch<-"seed";cancel:=make(chan struct{});ready:=make(chan struct{});done:=make(chan struct{});go func(){defer close(done);payload:=fmt.Sprintf("payload %d",i);close(ready);select{case ch<-payload:valid=false;case <-cancel:}}();<-ready;close(cancel);<-done;if <-ch!="seed"{valid=false}};elapsed:=time.Since(t0).Nanoseconds();checksum:=n;if !valid{checksum=-1};fmt.Println(checksum);fmt.Printf("bench_ns=%d\n",elapsed)}
