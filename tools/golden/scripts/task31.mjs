import {mkdirSync,writeFileSync,readFileSync} from 'node:fs';
import {resolve} from 'node:path';
import {pinnedSenpiRoot} from '../pin.mjs';
const source=pinnedSenpiRoot();
const destination=resolve(import.meta.dir,'../../../crates/maho-interactive/tests/golden');
mkdirSync(destination,{recursive:true});
const themeModule=await import(source+'/packages/coding-agent/src/modes/interactive/theme/theme.ts');
const {FooterComponent,formatTokens}=await import(source+'/packages/coding-agent/src/modes/interactive/components/footer.ts');
const {createFooterSession,createFooterData}=await import(source+'/packages/coding-agent/test/helpers/footer-test-fixtures.ts');
process.env.HOME='/home/test';
themeModule.initTheme('dark');
const snapshot={cwd:'/tmp/project',home:'/home/test',branch:'main',sessionName:'parity',cacheRead:8000,cacheWrite:0,cost:0.125,latestCacheHitRate:80,contextWindow:200000,contextPercent:12.3,contextTokens:null,modelId:'test-model',provider:'test',reasoning:true,thinkingLevel:'high',fastMode:false,subscription:false,providerCount:2,accountSuffix:'',extensionStatuses:{}};
const session=createFooterSession({cwd:snapshot.cwd,sessionName:snapshot.sessionName,modelId:snapshot.modelId,provider:snapshot.provider,reasoning:true,thinkingLevel:'high',usage:{input:2000,output:100,cacheRead:8000,cacheWrite:0,cost:{total:0.125}}});
const footer=new FooterComponent(session,createFooterData(2));
writeFileSync(destination+'/footer-input.json',JSON.stringify(snapshot,null,2)+'\n');
for(const width of [40,60,80,120,200])writeFileSync(destination+'/footer.'+width+'.ansi',footer.render(width).join('\n'));
const highlights=[];
for(const [language,code]of [['python','def hello(name):\n    return "hello" # greeting'],['rust','fn main() { let x = 42; }'],['json','{"name": "hello", "count": 42, "ok": true}'],[null,'plain text\nnext line']])highlights.push({language,code,lines:themeModule.highlightCode(code,language??undefined)});
writeFileSync(destination+'/highlight.json',JSON.stringify(highlights,null,2)+'\n');
const palettes=[];for(const name of ['dark','light','grok-day','grok-night']){const t=themeModule.getThemeByName(name);const colors=JSON.parse(readFileSync(source+'/packages/coding-agent/src/modes/interactive/theme/'+name+'.json','utf8')).colors;const fg={},bg={};for(const key of ["accent","border","borderAccent","borderMuted","success","error","warning","muted","dim","text","thinkingText","scrollbarTrack","scrollbarThumb","searchMatchText","userMessageText","customMessageText","customMessageLabel","toolTitle","toolOutput","mdHeading","mdLink","mdLinkUrl","skillMention","mdCode","mdCodeBlock","mdCodeBlockBorder","mdQuote","mdQuoteBorder","mdHr","mdListBullet","toolDiffAdded","toolDiffRemoved","toolDiffContext","syntaxComment","syntaxKeyword","syntaxFunction","syntaxVariable","syntaxString","syntaxNumber","syntaxType","syntaxOperator","syntaxPunctuation","thinkingOff","thinkingMinimal","thinkingLow","thinkingMedium","thinkingHigh","thinkingXhigh","thinkingMax","bashMode","selectedBg","searchMatchBg","userMessageBg","customMessageBg","toolPendingBg","toolSuccessBg","toolErrorBg"]){if(key.endsWith('Bg'))bg[key]=t.bg(key,'sample');else fg[key]=t.fg(key,'sample');}palettes.push({name,fg,bg});}writeFileSync(destination+'/palettes.json',JSON.stringify(palettes,null,2)+'\n');
const calls={tokens:[0,999,1000,6800,546000,1000000,1500000,2000000000].map(n=>[n,formatTokens(n)])};writeFileSync(destination+'/format-tokens.json',JSON.stringify(calls,null,2)+'\n');
console.log('Generated pinned task31 footer, theme, and highlight fixtures');


