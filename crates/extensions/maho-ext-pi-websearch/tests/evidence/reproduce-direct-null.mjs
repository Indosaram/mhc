const pin='ddf5f5d21de57ee3e80f1a6f96aff21a9dd34662';
const paths=['provider-endpoints.ts','config.ts'];const texts=[];
for(const path of paths){const response=await fetch(`https://raw.githubusercontent.com/code-yeongyu/pi-websearch/${pin}/src/websearch/${path}`);if(!response.ok)throw new Error(`Source ${response.status}`);texts.push(await response.text());}
const source=texts.map(text=>new Bun.Transpiler({loader:'ts'}).transformSync(text).replace(/^import .*?from .*?;\s*$/gm,'')).join('\n');
const script=source+`\nconsole.log(JSON.stringify(['weight','timeoutMs'].map(key=>{const input={strategy:'priority',fallback:true,auto:true,providers:[{provider:'duckduckgo-html',[key]:null}]};return {key,direct:validateWebsearchConfig(input),loaded:configFromObject(input)};})));`;
const child=Bun.spawn(['node','--input-type=module','-e',script],{stdout:'pipe',stderr:'inherit'});
const output=await new Response(child.stdout).text();const exit=await child.exited;const cases=JSON.parse(output);
if(exit!==0||cases.some(x=>x.direct.ok!==false||x.direct.reason!=='invalid_config'||x.loaded.providers[0][x.key]!==undefined))throw new Error('Null boundary mismatch');
console.log(JSON.stringify({pin,sources:paths.map((path,index)=>({path,sha256:new Bun.CryptoHasher('sha256').update(texts[index]).digest('hex')})),cases,exit}));
