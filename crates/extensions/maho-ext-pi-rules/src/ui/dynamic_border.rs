use maho_tui::tui::Component;

pub struct DynamicBorder<F:Fn(&str)->String>{color:F}
impl<F:Fn(&str)->String> DynamicBorder<F>{pub fn new(color:F)->Self{Self{color}}}
impl<F:Fn(&str)->String> Component for DynamicBorder<F>{
    fn render(&mut self,width:usize)->Vec<String>{vec![(self.color)(&"\u{2500}".repeat(width.max(1)))]}
}
