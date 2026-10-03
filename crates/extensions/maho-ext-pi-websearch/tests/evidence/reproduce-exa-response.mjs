const pin='ddf5f5d21de57ee3e80f1a6f96aff21a9dd34662';
const paths=['shared.ts','exa.ts'];const texts=[];
for(const path of paths){const response=await fetch(`https://raw.githubusercontent.com/code-yeongyu/pi-websearch/${pin}/src/websearch/providers/${path}`);if(!response.ok)throw new Error(`Source ${response.status}`);texts.push(await response.text());}
const fixture=JSON.parse(await Bun.file(new URL('../fixtures/pinned-exa-response-boundaries.json',import.meta.url)).text());
const code=texts.map(text=>new Bun.Transpiler({loader:'ts'}).transformSync(text).replace(/^import .*?from .*?;\s*$/gm,'')).join('\n');
const script=code+'\nconsole.log(JSON.stringify('+JSON.stringify(fixture.map(x=>x.input))+'.map(input=>({input,output:exaProvider.normalizeResponse(input)}))));';
const child=Bun.spawn(['node','--input-type=module','-e',script],{stdout:'pipe',stderr:'inherit'});
const output=await new Response(child.stdout).text();const exit=await child.exited;
if(exit!==0||JSON.stringify(JSON.parse(output))!==JSON.stringify(fixture))throw new Error('Exa corpus mismatch');
console.log(JSON.stringify({pin,sources:paths.map((path,index)=>({path,sha256:new Bun.CryptoHasher('sha256').update(texts[index]).digest('hex')})),cases:fixture.length,exit}));
