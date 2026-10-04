const pin='12ad906f0b29e949ebbd1f89d8f85789578aa6e6';const paths=['errors.ts','parser.ts'];const texts=[];
for(const path of paths){const response=await fetch(`https://raw.githubusercontent.com/code-yeongyu/pi-rules/${pin}/src/rules/${path}`);if(!response.ok)throw new Error(`Source ${response.status}`);texts.push(await response.text());}
const source=texts.map(text=>new Bun.Transpiler({loader:'ts'}).transformSync(text).replace(/^import .*?from .*?;\s*$/gm,'')).join('\n');
const inputs=["---\ndescription: '\n---\nbody","---\nglobs: '\n---\nbody"];
const child=Bun.spawn(['node','--input-type=module','-e',source+'\nconsole.log(JSON.stringify('+JSON.stringify(inputs)+'.map(input=>parseRule(input))));'],{stdout:'pipe',stderr:'inherit'});
const cases=JSON.parse(await new Response(child.stdout).text());const exit=await child.exited;
if(exit!==0||cases[0].frontmatter.description!==''||cases[1].frontmatter.globs!==''||cases.some(x=>x.body!=='body'||x.diagnostic!==undefined))throw new Error('Scalar mismatch');
console.log(JSON.stringify({pin,sources:paths.map((path,index)=>({path,sha256:new Bun.CryptoHasher('sha256').update(texts[index]).digest('hex')})),cases,exit}));
