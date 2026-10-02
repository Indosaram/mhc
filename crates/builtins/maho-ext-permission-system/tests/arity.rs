use maho_ext_permission_system::arity::prefix;
#[test]
fn arity_0() {
 assert_eq!(prefix(&["unknown","command","subcommand"]),vec!["unknown".to_owned()]);
 assert_eq!(prefix(&["touch","foo.txt"]),vec!["touch".to_owned()]);
}

#[test]
fn arity_1() {
 assert_eq!(prefix(&["git","checkout","main"]),vec!["git".to_owned(),"checkout".to_owned()]);
 assert_eq!(prefix(&["docker","run","nginx"]),vec!["docker".to_owned(),"run".to_owned()]);
}

#[test]
fn arity_2() {
 assert_eq!(prefix(&["aws","s3","ls","my-bucket"]),vec!["aws".to_owned(),"s3".to_owned(),"ls".to_owned()]);
 assert_eq!(prefix(&["npm","run","dev","script"]),vec!["npm".to_owned(),"run".to_owned(),"dev".to_owned()]);
}

#[test]
fn arity_3() {
 assert_eq!(prefix(&["docker","compose","up","service"]),vec!["docker".to_owned(),"compose".to_owned(),"up".to_owned()]);
 assert_eq!(prefix(&["consul","kv","get","config"]),vec!["consul".to_owned(),"kv".to_owned(),"get".to_owned()]);
}

#[test]
fn arity_4() {
 assert_eq!(prefix(&["git","checkout"]),vec!["git".to_owned(),"checkout".to_owned()]);
 assert_eq!(prefix(&["npm","run","dev"]),vec!["npm".to_owned(),"run".to_owned(),"dev".to_owned()]);
}

#[test]
fn arity_5() {
 assert_eq!(prefix(&[]),Vec::<String>::new());
 assert_eq!(prefix(&["single"]),vec!["single".to_owned()]);
 assert_eq!(prefix(&["git"]),vec!["git".to_owned()]);
}

#[test]
fn arity_6() {
 assert_eq!(prefix(&["git","commit","-m","x"]),vec!["git".to_owned(),"commit".to_owned()]);
}

#[test]
fn arity_7() {
 assert_eq!(prefix(&["npm","run","dev"]),vec!["npm".to_owned(),"run".to_owned(),"dev".to_owned()]);
}

#[test]
fn arity_8() {
 assert_eq!(prefix(&["rm","-rf","/"]),vec!["rm".to_owned()]);
}

#[test]
fn arity_9() {
 assert_eq!(prefix(&["docker","compose","up","-d"]),vec!["docker".to_owned(),"compose".to_owned(),"up".to_owned()]);
}

#[test]
fn arity_10() {
 assert_eq!(prefix(&["cargo","add","tokio"]),vec!["cargo".to_owned(),"add".to_owned(),"tokio".to_owned()]);
}

#[test]
fn arity_11() {
 assert_eq!(prefix(&["bun","run","dev"]),vec!["bun".to_owned(),"run".to_owned(),"dev".to_owned()]);
}

#[test]
fn arity_12() {
 assert_eq!(prefix(&["pnpm","dlx","create-next-app"]),vec!["pnpm".to_owned(),"dlx".to_owned(),"create-next-app".to_owned()]);
}

#[test]
fn arity_13() {
 assert_eq!(prefix(&["yarn","dlx","create-react-app"]),vec!["yarn".to_owned(),"dlx".to_owned(),"create-react-app".to_owned()]);
}

#[test]
fn arity_14() {
 assert_eq!(prefix(&["terraform","workspace","select","prod"]),vec!["terraform".to_owned(),"workspace".to_owned(),"select".to_owned()]);
}

#[test]
fn arity_15() {
 assert_eq!(prefix(&["kubectl","kustomize","overlays/dev"]),vec!["kubectl".to_owned(),"kustomize".to_owned(),"overlays/dev".to_owned()]);
}

#[test]
fn arity_16() {
 assert_eq!(prefix(&["eksctl","create","cluster"]),vec!["eksctl".to_owned(),"create".to_owned(),"cluster".to_owned()]);
}

#[test]
fn arity_17() {
 assert_eq!(prefix(&["kind","create","cluster"]),vec!["kind".to_owned(),"create".to_owned(),"cluster".to_owned()]);
}

#[test]
fn arity_18() {
 assert_eq!(prefix(&["openssl","req","-new","-key","key.pem"]),vec!["openssl".to_owned(),"req".to_owned(),"-new".to_owned()]);
}

#[test]
fn arity_19() {
 assert_eq!(prefix(&["ip","addr","show"]),vec!["ip".to_owned(),"addr".to_owned(),"show".to_owned()]);
}

#[test]
fn arity_20() {
 assert_eq!(prefix(&["mc","admin","info","myminio"]),vec!["mc".to_owned(),"admin".to_owned(),"info".to_owned()]);
}

#[test]
fn arity_21() {
 assert_eq!(prefix(&["pulumi","stack","output"]),vec!["pulumi".to_owned(),"stack".to_owned(),"output".to_owned()]);
}

#[test]
fn arity_22() {
 assert_eq!(prefix(&["vault","kv","get","secret/api"]),vec!["vault".to_owned(),"kv".to_owned(),"get".to_owned()]);
}

#[test]
fn arity_23() {
 assert_eq!(prefix(&["deno","task","dev"]),vec!["deno".to_owned(),"task".to_owned(),"dev".to_owned()]);
}

#[test]
fn arity_24() {
 assert_eq!(prefix(&["pip","install","numpy"]),vec!["pip".to_owned(),"install".to_owned()]);
}

#[test]
fn arity_25() {
 assert_eq!(prefix(&["python","-m","venv","env"]),vec!["python".to_owned(),"-m".to_owned()]);
}

#[test]
fn arity_26() {
 assert_eq!(prefix(&["cd","/path/to/dir"]),vec!["cd".to_owned()]);
}

#[test]
fn arity_27() {
 assert_eq!(prefix(&["ls","-la"]),vec!["ls".to_owned()]);
}

#[test]
fn arity_28() {
 assert_eq!(prefix(&["cat","file.txt"]),vec!["cat".to_owned()]);
}

#[test]
fn arity_29() {
 assert_eq!(prefix(&["mkdir","new-dir"]),vec!["mkdir".to_owned()]);
}

#[test]
fn arity_30() {
 assert_eq!(prefix(&["mv","old.txt","new.txt"]),vec!["mv".to_owned()]);
}

#[test]
fn arity_31() {
 assert_eq!(prefix(&["cp","source.txt","dest.txt"]),vec!["cp".to_owned()]);
}

#[test]
fn arity_32() {
 assert_eq!(prefix(&["chmod","755","script.sh"]),vec!["chmod".to_owned()]);
}

#[test]
fn arity_33() {
 assert_eq!(prefix(&["chown","user:group","file.txt"]),vec!["chown".to_owned()]);
}

#[test]
fn arity_34() {
 assert_eq!(prefix(&["ln","-s","source","target"]),vec!["ln".to_owned()]);
}

#[test]
fn arity_35() {
 assert_eq!(prefix(&["tail","-f","log.txt"]),vec!["tail".to_owned()]);
}

#[test]
fn arity_36() {
 assert_eq!(prefix(&["grep","pattern","file.txt"]),vec!["grep".to_owned()]);
}

#[test]
fn arity_37() {
 assert_eq!(prefix(&["ps","aux"]),vec!["ps".to_owned()]);
}

#[test]
fn arity_38() {
 assert_eq!(prefix(&["kill","1234"]),vec!["kill".to_owned()]);
}

#[test]
fn arity_39() {
 assert_eq!(prefix(&["killall","process"]),vec!["killall".to_owned()]);
}

#[test]
fn arity_40() {
 assert_eq!(prefix(&["which","node"]),vec!["which".to_owned()]);
}

#[test]
fn arity_41() {
 assert_eq!(prefix(&["echo","hello","world"]),vec!["echo".to_owned()]);
}

#[test]
fn arity_42() {
 assert_eq!(prefix(&["export","PATH=/usr/bin"]),vec!["export".to_owned()]);
}

#[test]
fn arity_43() {
 assert_eq!(prefix(&["unset","VAR"]),vec!["unset".to_owned()]);
}

#[test]
fn arity_44() {
 assert_eq!(prefix(&["source","~/.bashrc"]),vec!["source".to_owned()]);
}

#[test]
fn arity_45() {
 assert_eq!(prefix(&["sleep","5"]),vec!["sleep".to_owned()]);
}

#[test]
fn arity_46() {
 assert_eq!(prefix(&["pwd"]),vec!["pwd".to_owned()]);
}

#[test]
fn arity_47() {
 assert_eq!(prefix(&["rmdir","empty-dir"]),vec!["rmdir".to_owned()]);
}

#[test]
fn arity_48() {
 assert_eq!(prefix(&["env"]),vec!["env".to_owned()]);
}

#[test]
fn arity_49() {
 assert_eq!(prefix(&["aws","s3","ls"]),vec!["aws".to_owned(),"s3".to_owned(),"ls".to_owned()]);
}

#[test]
fn arity_50() {
 assert_eq!(prefix(&["az","storage","blob","list"]),vec!["az".to_owned(),"storage".to_owned(),"blob".to_owned()]);
}

#[test]
fn arity_51() {
 assert_eq!(prefix(&["gcloud","compute","instances","list"]),vec!["gcloud".to_owned(),"compute".to_owned(),"instances".to_owned()]);
}

#[test]
fn arity_52() {
 assert_eq!(prefix(&["gh","pr","list"]),vec!["gh".to_owned(),"pr".to_owned(),"list".to_owned()]);
}

#[test]
fn arity_53() {
 assert_eq!(prefix(&["doctl","kubernetes","cluster","list"]),vec!["doctl".to_owned(),"kubernetes".to_owned(),"cluster".to_owned()]);
}

#[test]
fn arity_54() {
 assert_eq!(prefix(&["sfdx","force:org:list"]),vec!["sfdx".to_owned(),"force:org:list".to_owned()]);
}

#[test]
fn arity_55() {
 assert_eq!(prefix(&["brew","install","node"]),vec!["brew".to_owned(),"install".to_owned()]);
}

#[test]
fn arity_56() {
 assert_eq!(prefix(&["bazel","build"]),vec!["bazel".to_owned(),"build".to_owned()]);
}

#[test]
fn arity_57() {
 assert_eq!(prefix(&["cargo","build"]),vec!["cargo".to_owned(),"build".to_owned()]);
}

#[test]
fn arity_58() {
 assert_eq!(prefix(&["cargo","run","main"]),vec!["cargo".to_owned(),"run".to_owned(),"main".to_owned()]);
}

#[test]
fn arity_59() {
 assert_eq!(prefix(&["cdk","deploy"]),vec!["cdk".to_owned(),"deploy".to_owned()]);
}

#[test]
fn arity_60() {
 assert_eq!(prefix(&["cf","push","app"]),vec!["cf".to_owned(),"push".to_owned()]);
}

#[test]
fn arity_61() {
 assert_eq!(prefix(&["cmake","build"]),vec!["cmake".to_owned(),"build".to_owned()]);
}

#[test]
fn arity_62() {
 assert_eq!(prefix(&["composer","require","laravel"]),vec!["composer".to_owned(),"require".to_owned()]);
}

#[test]
fn arity_63() {
 assert_eq!(prefix(&["consul","members"]),vec!["consul".to_owned(),"members".to_owned()]);
}

#[test]
fn arity_64() {
 assert_eq!(prefix(&["crictl","ps"]),vec!["crictl".to_owned(),"ps".to_owned()]);
}

#[test]
fn arity_65() {
 assert_eq!(prefix(&["deno","run","server.ts"]),vec!["deno".to_owned(),"run".to_owned()]);
}

#[test]
fn arity_66() {
 assert_eq!(prefix(&["docker","run","nginx"]),vec!["docker".to_owned(),"run".to_owned()]);
}

#[test]
fn arity_67() {
 assert_eq!(prefix(&["docker","builder","prune"]),vec!["docker".to_owned(),"builder".to_owned(),"prune".to_owned()]);
}

#[test]
fn arity_68() {
 assert_eq!(prefix(&["docker","container","ls"]),vec!["docker".to_owned(),"container".to_owned(),"ls".to_owned()]);
}

#[test]
fn arity_69() {
 assert_eq!(prefix(&["docker","image","prune"]),vec!["docker".to_owned(),"image".to_owned(),"prune".to_owned()]);
}

#[test]
fn arity_70() {
 assert_eq!(prefix(&["docker","network","inspect"]),vec!["docker".to_owned(),"network".to_owned(),"inspect".to_owned()]);
}

#[test]
fn arity_71() {
 assert_eq!(prefix(&["docker","volume","ls"]),vec!["docker".to_owned(),"volume".to_owned(),"ls".to_owned()]);
}

#[test]
fn arity_72() {
 assert_eq!(prefix(&["eksctl","get","clusters"]),vec!["eksctl".to_owned(),"get".to_owned()]);
}

#[test]
fn arity_73() {
 assert_eq!(prefix(&["firebase","deploy"]),vec!["firebase".to_owned(),"deploy".to_owned()]);
}

#[test]
fn arity_74() {
 assert_eq!(prefix(&["flyctl","deploy"]),vec!["flyctl".to_owned(),"deploy".to_owned()]);
}

#[test]
fn arity_75() {
 assert_eq!(prefix(&["git","config","user.name"]),vec!["git".to_owned(),"config".to_owned(),"user.name".to_owned()]);
}

#[test]
fn arity_76() {
 assert_eq!(prefix(&["git","remote","add","origin"]),vec!["git".to_owned(),"remote".to_owned(),"add".to_owned()]);
}

#[test]
fn arity_77() {
 assert_eq!(prefix(&["git","stash","pop"]),vec!["git".to_owned(),"stash".to_owned(),"pop".to_owned()]);
}

#[test]
fn arity_78() {
 assert_eq!(prefix(&["go","build"]),vec!["go".to_owned(),"build".to_owned()]);
}

#[test]
fn arity_79() {
 assert_eq!(prefix(&["gradle","build"]),vec!["gradle".to_owned(),"build".to_owned()]);
}

#[test]
fn arity_80() {
 assert_eq!(prefix(&["helm","install","mychart"]),vec!["helm".to_owned(),"install".to_owned()]);
}

#[test]
fn arity_81() {
 assert_eq!(prefix(&["heroku","logs"]),vec!["heroku".to_owned(),"logs".to_owned()]);
}

#[test]
fn arity_82() {
 assert_eq!(prefix(&["hugo","new","site","blog"]),vec!["hugo".to_owned(),"new".to_owned()]);
}

#[test]
fn arity_83() {
 assert_eq!(prefix(&["ip","link","show"]),vec!["ip".to_owned(),"link".to_owned(),"show".to_owned()]);
}

#[test]
fn arity_84() {
 assert_eq!(prefix(&["ip","link","set","eth0","up"]),vec!["ip".to_owned(),"link".to_owned(),"set".to_owned()]);
}

#[test]
fn arity_85() {
 assert_eq!(prefix(&["ip","netns","exec","foo","bash"]),vec!["ip".to_owned(),"netns".to_owned(),"exec".to_owned()]);
}

#[test]
fn arity_86() {
 assert_eq!(prefix(&["ip","route","add","default","via","1.1.1.1"]),vec!["ip".to_owned(),"route".to_owned(),"add".to_owned()]);
}

#[test]
fn arity_87() {
 assert_eq!(prefix(&["kind","delete","cluster"]),vec!["kind".to_owned(),"delete".to_owned()]);
}

#[test]
fn arity_88() {
 assert_eq!(prefix(&["kubectl","get","pods"]),vec!["kubectl".to_owned(),"get".to_owned()]);
}

#[test]
fn arity_89() {
 assert_eq!(prefix(&["kubectl","rollout","restart","deploy/api"]),vec!["kubectl".to_owned(),"rollout".to_owned(),"restart".to_owned()]);
}

#[test]
fn arity_90() {
 assert_eq!(prefix(&["kustomize","build","."]),vec!["kustomize".to_owned(),"build".to_owned()]);
}

#[test]
fn arity_91() {
 assert_eq!(prefix(&["make","build"]),vec!["make".to_owned(),"build".to_owned()]);
}

#[test]
fn arity_92() {
 assert_eq!(prefix(&["mc","ls","myminio"]),vec!["mc".to_owned(),"ls".to_owned()]);
}

#[test]
fn arity_93() {
 assert_eq!(prefix(&["minikube","start"]),vec!["minikube".to_owned(),"start".to_owned()]);
}

#[test]
fn arity_94() {
 assert_eq!(prefix(&["mongosh","test"]),vec!["mongosh".to_owned(),"test".to_owned()]);
}

#[test]
fn arity_95() {
 assert_eq!(prefix(&["mysql","-u","root"]),vec!["mysql".to_owned(),"-u".to_owned()]);
}

#[test]
fn arity_96() {
 assert_eq!(prefix(&["mvn","compile"]),vec!["mvn".to_owned(),"compile".to_owned()]);
}

#[test]
fn arity_97() {
 assert_eq!(prefix(&["ng","generate","component","home"]),vec!["ng".to_owned(),"generate".to_owned()]);
}

#[test]
fn arity_98() {
 assert_eq!(prefix(&["npm","install"]),vec!["npm".to_owned(),"install".to_owned()]);
}

#[test]
fn arity_99() {
 assert_eq!(prefix(&["npm","exec","vite"]),vec!["npm".to_owned(),"exec".to_owned(),"vite".to_owned()]);
}

#[test]
fn arity_100() {
 assert_eq!(prefix(&["npm","init","vue"]),vec!["npm".to_owned(),"init".to_owned(),"vue".to_owned()]);
}

#[test]
fn arity_101() {
 assert_eq!(prefix(&["npm","view","react","version"]),vec!["npm".to_owned(),"view".to_owned(),"react".to_owned()]);
}

#[test]
fn arity_102() {
 assert_eq!(prefix(&["nvm","use","18"]),vec!["nvm".to_owned(),"use".to_owned()]);
}

#[test]
fn arity_103() {
 assert_eq!(prefix(&["nx","build"]),vec!["nx".to_owned(),"build".to_owned()]);
}

#[test]
fn arity_104() {
 assert_eq!(prefix(&["openssl","genrsa","2048"]),vec!["openssl".to_owned(),"genrsa".to_owned()]);
}

#[test]
fn arity_105() {
 assert_eq!(prefix(&["openssl","x509","-in","cert.pem"]),vec!["openssl".to_owned(),"x509".to_owned(),"-in".to_owned()]);
}

#[test]
fn arity_106() {
 assert_eq!(prefix(&["pipenv","install","flask"]),vec!["pipenv".to_owned(),"install".to_owned()]);
}

#[test]
fn arity_107() {
 assert_eq!(prefix(&["pnpm","install"]),vec!["pnpm".to_owned(),"install".to_owned()]);
}

#[test]
fn arity_108() {
 assert_eq!(prefix(&["pnpm","exec","vite"]),vec!["pnpm".to_owned(),"exec".to_owned(),"vite".to_owned()]);
}

#[test]
fn arity_109() {
 assert_eq!(prefix(&["poetry","add","requests"]),vec!["poetry".to_owned(),"add".to_owned()]);
}

#[test]
fn arity_110() {
 assert_eq!(prefix(&["podman","run","alpine"]),vec!["podman".to_owned(),"run".to_owned()]);
}

#[test]
fn arity_111() {
 assert_eq!(prefix(&["podman","container","ls"]),vec!["podman".to_owned(),"container".to_owned(),"ls".to_owned()]);
}

#[test]
fn arity_112() {
 assert_eq!(prefix(&["podman","image","prune"]),vec!["podman".to_owned(),"image".to_owned(),"prune".to_owned()]);
}

#[test]
fn arity_113() {
 assert_eq!(prefix(&["psql","-d","mydb"]),vec!["psql".to_owned(),"-d".to_owned()]);
}

#[test]
fn arity_114() {
 assert_eq!(prefix(&["pulumi","up"]),vec!["pulumi".to_owned(),"up".to_owned()]);
}

#[test]
fn arity_115() {
 assert_eq!(prefix(&["pyenv","install","3.11"]),vec!["pyenv".to_owned(),"install".to_owned()]);
}

#[test]
fn arity_116() {
 assert_eq!(prefix(&["rake","db:migrate"]),vec!["rake".to_owned(),"db:migrate".to_owned()]);
}

#[test]
fn arity_117() {
 assert_eq!(prefix(&["rbenv","install","3.2.0"]),vec!["rbenv".to_owned(),"install".to_owned()]);
}

#[test]
fn arity_118() {
 assert_eq!(prefix(&["redis-cli","ping"]),vec!["redis-cli".to_owned(),"ping".to_owned()]);
}

#[test]
fn arity_119() {
 assert_eq!(prefix(&["rustup","update"]),vec!["rustup".to_owned(),"update".to_owned()]);
}

#[test]
fn arity_120() {
 assert_eq!(prefix(&["serverless","invoke"]),vec!["serverless".to_owned(),"invoke".to_owned()]);
}

#[test]
fn arity_121() {
 assert_eq!(prefix(&["skaffold","dev"]),vec!["skaffold".to_owned(),"dev".to_owned()]);
}

#[test]
fn arity_122() {
 assert_eq!(prefix(&["sls","deploy"]),vec!["sls".to_owned(),"deploy".to_owned()]);
}

#[test]
fn arity_123() {
 assert_eq!(prefix(&["sst","deploy"]),vec!["sst".to_owned(),"deploy".to_owned()]);
}

#[test]
fn arity_124() {
 assert_eq!(prefix(&["swift","build"]),vec!["swift".to_owned(),"build".to_owned()]);
}

#[test]
fn arity_125() {
 assert_eq!(prefix(&["systemctl","restart","nginx"]),vec!["systemctl".to_owned(),"restart".to_owned()]);
}

#[test]
fn arity_126() {
 assert_eq!(prefix(&["terraform","apply"]),vec!["terraform".to_owned(),"apply".to_owned()]);
}

#[test]
fn arity_127() {
 assert_eq!(prefix(&["tmux","new","-s","dev"]),vec!["tmux".to_owned(),"new".to_owned()]);
}

#[test]
fn arity_128() {
 assert_eq!(prefix(&["turbo","run","build"]),vec!["turbo".to_owned(),"run".to_owned()]);
}

#[test]
fn arity_129() {
 assert_eq!(prefix(&["ufw","allow","22"]),vec!["ufw".to_owned(),"allow".to_owned()]);
}

#[test]
fn arity_130() {
 assert_eq!(prefix(&["vault","login"]),vec!["vault".to_owned(),"login".to_owned()]);
}

#[test]
fn arity_131() {
 assert_eq!(prefix(&["vault","auth","list"]),vec!["vault".to_owned(),"auth".to_owned(),"list".to_owned()]);
}

#[test]
fn arity_132() {
 assert_eq!(prefix(&["vercel","deploy"]),vec!["vercel".to_owned(),"deploy".to_owned()]);
}

#[test]
fn arity_133() {
 assert_eq!(prefix(&["volta","install","node"]),vec!["volta".to_owned(),"install".to_owned()]);
}

#[test]
fn arity_134() {
 assert_eq!(prefix(&["wp","plugin","install"]),vec!["wp".to_owned(),"plugin".to_owned()]);
}

#[test]
fn arity_135() {
 assert_eq!(prefix(&["yarn","add","react"]),vec!["yarn".to_owned(),"add".to_owned()]);
}

#[test]
fn arity_136() {
 assert_eq!(prefix(&["yarn","run","dev"]),vec!["yarn".to_owned(),"run".to_owned(),"dev".to_owned()]);
}

#[test]
fn arity_137() {
 assert_eq!(prefix(&["bun","install"]),vec!["bun".to_owned(),"install".to_owned()]);
}

#[test]
fn arity_138() {
 assert_eq!(prefix(&["bun","x","vite"]),vec!["bun".to_owned(),"x".to_owned(),"vite".to_owned()]);
}

