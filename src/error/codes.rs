//! XSLT 2.0 error codes (Appendix E of the specification).
//!
//! Generated from the spec text; one constant per code, with the spec's own wording as
//! documentation. [`describe`] looks a code up at run time.
//!
//! The generator is not in the repository. The rustdoc escapes in the doc comments
//! (`\[xsl:…\]` for the spec's optional-prefix attribute names, `<…>` around a URL, a
//! code span around a URL the one-line summary cuts off) were added by hand; the
//! wording is the spec's.

/// (code, first sentence(s) of the spec description).
pub static CODES: &[(&str, &str)] = &[
    ("XTDE0030", "It is a non-recoverable dynamic error if the effective value of an attribute written using curly brackets, in a position where an attribute value template is permitted, is a value that is not one of the permitted values for that attribute. If the processor is able to detect the error statically (for example, when any XPath expressions within the curly brackets can be evaluated statically), then the processor may optionally signal this as a static error."),
    ("XTDE0040", "It is a non-recoverable dynamic error if the invocation of the stylesheet specifies a template name that does not match the expanded-QName of a named template defined in the stylesheet ."),
    ("XTDE0045", "It is a non-recoverable dynamic error if the invocation of the stylesheet specifies an initial mode (other than the default mode) that does not match the expanded-QName in the mode attribute of any template defined in the stylesheet ."),
    ("XTDE0047", "It is a non-recoverable dynamic error if the invocation of the stylesheet specifies both an initial mode and an initial template."),
    ("XTDE0050", "It is a non-recoverable dynamic error if the stylesheet that is invoked declares a visible stylesheet parameter with required=\"yes\" and no value for this parameter is supplied during the invocation of the stylesheet. A stylesheet parameter is visible if it is not masked by another global variable or parameter with the same name and higher import precedence ."),
    ("XTDE0060", "It is a non-recoverable dynamic error if the initial template defines a template parameter that specifies required=\"yes\" ."),
    ("XTDE0160", "If an implementation does not support backwards-compatible behavior, then it is a non-recoverable dynamic error if any element is evaluated that enables backwards-compatible behavior."),
    ("XTDE0290", "Where the result of evaluating an XPath expression (or an attribute value template) is required to be a lexical QName , then unless otherwise specified it is a non-recoverable dynamic error if the defining element has no namespace node whose name matches the prefix of the lexical QName . This error may be signaled as a static error if the value of the expression can be determined statically."),
    ("XTDE0410", "It is a non-recoverable dynamic error if the result sequence used to construct the content of an element node contains a namespace node or attribute node that is preceded in the sequence by a node that is neither a namespace node nor an attribute node."),
    ("XTDE0420", "It is a non-recoverable dynamic error if the result sequence used to construct the content of a document node contains a namespace node or attribute node."),
    ("XTDE0430", "It is a non-recoverable dynamic error if the result sequence contains two or more namespace nodes having the same name but different string values (that is, namespace nodes that map the same prefix to different namespace URIs)."),
    ("XTDE0440", "It is a non-recoverable dynamic error if the result sequence contains a namespace node with no name and the element node being constructed has a null namespace URI (that is, it is an error to define a default namespace when the element is in no namespace)."),
    ("XTDE0560", "It is a non-recoverable dynamic error if xsl:apply-imports or xsl:next-match is evaluated when the current template rule is null."),
    ("XTDE0610", "If an optional parameter has no select attribute and has an empty sequence constructor , and if there is an as attribute, then the default value of the parameter is an empty sequence. If the empty sequence is not a valid instance of the required type defined in the as attribute, then the parameter is treated as a required parameter, which means that it is a non-recoverable dynamic error if the caller supplies no value for the parameter."),
    ("XTDE0640", "In general, a circularity in a stylesheet is a non-recoverable dynamic error ."),
    ("XTDE0700", "In other cases, [with xsl:apply-templates , xsl:apply-imports , and xsl:next-match , or xsl:call-template with tunnel parameters ] it is a non-recoverable dynamic error if the template that is invoked declares a template parameter with required=\"yes\" and no value for this parameter is supplied by the calling instruction."),
    ("XTDE0820", "It is a non-recoverable dynamic error if the effective value of the name attribute [of the xsl:element instruction] is not a lexical QName ."),
    ("XTDE0830", "In the case of an xsl:element instruction with no namespace attribute, it is a non-recoverable dynamic error if the effective value of the name attribute is a QName whose prefix is not declared in an in-scope namespace declaration for the xsl:element instruction."),
    ("XTDE0835", "It is a non-recoverable dynamic error if the effective value of the namespace attribute [of the xsl:element instruction] is not in the lexical space of the xs:anyURI data type or if it is the string http://www.w3.org/2000/xmlns/ ."),
    ("XTDE0850", "It is a non-recoverable dynamic error if the effective value of the name attribute [of an xsl:attribute instruction] is not a lexical QName ."),
    ("XTDE0855", "In the case of an xsl:attribute instruction with no namespace attribute, it is a non-recoverable dynamic error if the effective value of the name attribute is the string xmlns ."),
    ("XTDE0860", "In the case of an xsl:attribute instruction with no namespace attribute, it is a non-recoverable dynamic error if the effective value of the name attribute is a lexical QName whose prefix is not declared in an in-scope namespace declaration for the xsl:attribute instruction."),
    ("XTDE0865", "It is a non-recoverable dynamic error if the effective value of the namespace attribute [of the xsl:attribute instruction] is not in the lexical space of the xs:anyURI data type or if it is the string http://www.w3.org/2000/xmlns/ ."),
    ("XTDE0890", "It is a non-recoverable dynamic error if the effective value of the name attribute [of the xsl:processing-instruction instruction] is not both an NCName Names and a PITarget XML ."),
    ("XTDE0905", "It is a non-recoverable dynamic error if the string value of the new namespace node is not valid in the lexical space of the data type xs:anyURI , or if it is the string http://www.w3.org/2000/xmlns/ ."),
    ("XTDE0920", "It is a non-recoverable dynamic error if the effective value of the name attribute [of the xsl:namespace instruction] is neither a zero-length string nor an NCName Names , or if it is xmlns ."),
    ("XTDE0925", "It is a non-recoverable dynamic error if the xsl:namespace instruction generates a namespace node whose name is xml and whose string value is not http://www.w3.org/XML/1998/namespace , or a namespace node whose string value is http://www.w3.org/XML/1998/namespace and whose name is not xml ."),
    ("XTDE0930", "It is a non-recoverable dynamic error if evaluating the select attribute or the contained sequence constructor of an xsl:namespace instruction results in a zero-length string."),
    ("XTDE0980", "It is a non-recoverable dynamic error if any undiscarded item in the atomized sequence supplied as the value of the value attribute of xsl:number cannot be converted to an integer, or if the resulting integer is less than 0 (zero)."),
    ("XTDE1030", "It is a non-recoverable dynamic error if, for any sort key component , the set of sort key values evaluated for all the items in the initial sequence , after any type conversion requested, contains a pair of ordinary values for which the result of the XPath lt operator is an error."),
    ("XTDE1035", "It is a non-recoverable dynamic error if the collation attribute of xsl:sort (after resolving against the base URI) is not a URI that is recognized by the implementation as referring to a collation."),
    ("XTDE1110", "It is a non-recoverable dynamic error if the collation URI specified to xsl:for-each-group (after resolving against the base URI) is a collation that is not recognized by the implementation. (For notes, [see"),
    ("XTDE1140", "It is a non-recoverable dynamic error if the effective value of the regex attribute [of the xsl:analyze-string instruction] does not conform to the required syntax for regular expressions, as specified in [Functions and Operators] . If the regular expression is known statically (for example, if the attribute does not contain any expressions enclosed in curly brackets) then the processor may signal the error as a static error ."),
    ("XTDE1145", "It is a non-recoverable dynamic error if the effective value of the flags attribute [of the xsl:analyze-string instruction] has a value other than the values defined in [Functions and Operators] . If the value of the attribute is known statically (for example, if the attribute does not contain any expressions enclosed in curly brackets) then the processor may signal the error as a static error ."),
    ("XTDE1150", "It is a non-recoverable dynamic error if the effective value of the regex attribute [of the xsl:analyze-string instruction] is a regular expression that matches a zero-length string: or more specifically, if the regular expression $r and flags $f are such that matches(\"\", $r, $f) returns true. If the regular expression is known statically (for example, if the attribute does not contain any expressions enclosed in curly brackets) then the processor may signal the error as a static error ."),
    ("XTDE1170", "It is a non-recoverable dynamic error if a URI [supplied in the first argument to the unparsed-text function] contains a fragment identifier, or if it cannot be used to retrieve a resource containing text."),
    ("XTDE1190", "It is a non-recoverable dynamic error if a resource [retrieved using the unparsed-text function] contains octets that cannot be decoded into Unicode characters using the specified encoding, or if the resulting characters are not permitted XML characters. This includes the case where the processor does not support the requested encoding."),
    ("XTDE1200", "It is a non-recoverable dynamic error if the second argument of the unparsed-text function is omitted and the processor cannot infer the encoding using external information and the encoding is not UTF-8."),
    ("XTDE1260", "It is a non-recoverable dynamic error if the value [of the first argument to the key function] is not a valid QName, or if there is no namespace declaration in scope for the prefix of the QName, or if the name obtained by expanding the QName is not the same as the expanded name of any xsl:key declaration in the stylesheet . If the processor is able to detect the error statically (for example, when the argument is supplied as a string literal), then the processor may optionally signal this as a static error ."),
    ("XTDE1270", "It is a non-recoverable dynamic error to call the key function with two arguments if there is no context node , or if the root of the tree containing the context node is not a document node; or to call the function with three arguments if the root of the tree containing the node supplied in the third argument is not a document node."),
    ("XTDE1280", "It is a non-recoverable dynamic error if the name specified as the $decimal-format-name argument [ to the format-number function] is not a valid QName , or if its prefix has not been declared in an in-scope namespace declaration, or if the stylesheet does not contain a declaration of a decimal-format with a matching expanded-QName . If the processor is able to detect the error statically (for example, when the argument is supplied as a string literal), then the processor may optionally signal this as a static error ."),
    ("XTDE1310", "The picture string [supplied to the format-number function] must conform to the following rules. [ See full specification.] It is a non-recoverable dynamic error if the picture string does not satisfy these rules."),
    ("XTDE1340", "It is a non-recoverable dynamic error if the syntax of the picture [used for date/time formatting] is incorrect."),
    ("XTDE1350", "It is a non-recoverable dynamic error if a component specifier within the picture [used for date/time formatting] refers to components that are not available in the given type of $value , for example if the picture supplied to the format-time refers to the year, month, or day component."),
    ("XTDE1360", "If the current function is evaluated within an expression that is evaluated when the context item is undefined, a non-recoverable dynamic error occurs."),
    ("XTDE1370", "It is a non-recoverable dynamic error if the unparsed-entity-uri function is called when there is no context node , or when the root of the tree containing the context node is not a document node."),
    ("XTDE1380", "It is a non-recoverable dynamic error if the unparsed-entity-public-id function is called when there is no context node , or when the root of the tree containing the context node is not a document node."),
    ("XTDE1390", "It is a non-recoverable dynamic error if the value [supplied as the $property-name argument to the system-property function] is not a valid QName, or if there is no namespace declaration in scope for the prefix of the QName. If the processor is able to detect the error statically (for example, when the argument is supplied as a string literal), then the processor may optionally signal this as a static error ."),
    ("XTDE1400", "It is a non-recoverable dynamic error if the argument [passed to the function-available function] does not evaluate to a string that is a valid QName , or if there is no namespace declaration in scope for the prefix of the QName . If the processor is able to detect the error statically (for example, when the argument is supplied as a string literal), then the processor may optionally signal this as a static error ."),
    ("XTDE1420", "It is a non-recoverable dynamic error if the arguments supplied to a call on an extension function do not satisfy the rules defined for that particular extension function, or if the extension function reports an error, or if the result of the extension function cannot be converted to an XPath value."),
    ("XTDE1425", "When backwards compatible behavior is enabled, it is a non-recoverable dynamic error to evaluate an extension function call if no implementation of the extension function is available."),
    ("XTDE1428", "It is a non-recoverable dynamic error if the argument [passed to the type-available function] does not evaluate to a string that is a valid QName , or if there is no namespace declaration in scope for the prefix of the QName . If the processor is able to detect the error statically (for example, when the argument is supplied as a string literal), then the processor may optionally signal this as a static error ."),
    ("XTDE1440", "It is a non-recoverable dynamic error if the argument [passed to the element-available function] does not evaluate to a string that is a valid QName , or if there is no namespace declaration in scope for the prefix of the QName . If the processor is able to detect the error statically (for example, when the argument is supplied as a string literal), then the processor may optionally signal this as a static error ."),
    ("XTDE1450", "When a processor performs fallback for an extension instruction that is not recognized, if the instruction element has one or more xsl:fallback children, then the content of each of the xsl:fallback children must be evaluated; it is a non-recoverable dynamic error if it has no xsl:fallback children."),
    ("XTDE1460", "It is a non-recoverable dynamic error if the effective value of the format attribute [of an xsl:result-document element] is not a valid lexical QName , or if it does not match the expanded-QName of an output definition in the stylesheet . If the processor is able to detect the error statically (for example, when the format attribute contains no curly brackets), then the processor may optionally signal this as a static error ."),
    ("XTDE1480", "It is a non-recoverable dynamic error to evaluate the xsl:result-document instruction in temporary output state ."),
    ("XTDE1490", "It is a non-recoverable dynamic error for a transformation to generate two or more final result trees with the same URI."),
    ("XTDE1665", "A basic XSLT processor must raise a non-recoverable dynamic error if the input to the processor includes a node with a type annotation other than xs:untyped or xs:untypedAtomic , or an atomic value of a type other than those which a basic XSLT processor supports. F Checklist of Implementation-Defined Features (Non-Normative) This appendix provides a summary of XSLT language features whose effect is explicitly implementation-defined . The conformance rules (see 21 Conformance ) require vendors to provide documentation that explains how these choices have been exercised. The way in which an XSLT"),
    ("XTMM9000", "When a transformation is terminated by use of xsl:message terminate=\"yes\" , the effect is the same as when a non-recoverable dynamic error occurs during the transformation."),
    ("XTRE0270", "It is a recoverable dynamic error if this [the process of finding an xsl:strip-space or xsl:preserve-space declaration to match an element in the source document] leaves more than one match, unless all the matched declarations are equivalent (that is, they are all xsl:strip-space or they are all xsl:preserve-space ). Action: The optional recovery action is to select, from the matches that are left, the one that occurs last in declaration order ."),
    ("XTRE0540", "It is a recoverable dynamic error if the conflict resolution algorithm for template rules leaves more than one matching template rule. Action: The optional recovery action is to select, from the matching template rules that are left, the one that occurs last in declaration order ."),
    ("XTRE0795", "It is a recoverable dynamic error if the name of a constructed attribute is xml:space and the value is not either default or preserve . Action: The optional recovery action is to construct the attribute with the value as requested."),
    ("XTRE1160", "When a URI reference [supplied to the document function] contains a fragment identifier, it is a recoverable dynamic error if the media type is not one that is recognized by the processor, or if the fragment identifier does not conform to the rules for fragment identifiers for that media type, or if the fragment identifier selects something other than a sequence of nodes (for example, if it selects a range of characters within a text node). Action: The optional recovery action is to ignore the fragment identifier and return the document node."),
    ("XTRE1495", "It is a recoverable dynamic error for a transformation to generate two or more final result trees with URIs that identify the same physical resource. The optional recovery action is implementation-dependent , since it may be impossible for the processor to detect the error."),
    ("XTRE1500", "It is a recoverable dynamic error for a stylesheet to write to an external resource and read from the same resource during a single transformation, whether or not the same URI is used to access the resource in both cases. Action: The optional recovery action is implementation-dependent : implementations are not required to detect the error condition. Note that if the error is not detected, it is undefined whether the document that is read from the resource reflects its state before or after the result tree is written."),
    ("XTRE1620", "It is a recoverable dynamic error if an xsl:value-of or xsl:text instruction specifies that output escaping is to be disabled and the implementation does not support this. Action: The optional recovery action is to ignore the disable-output-escaping attribute."),
    ("XTRE1630", "It is a recoverable dynamic error if an xsl:value-of or xsl:text instruction specifies that output escaping is to be disabled when writing to a final result tree that is not being serialized. Action: The optional recovery action is to ignore the disable-output-escaping attribute."),
    ("XTSE0010", "A static error is signaled if an XSLT-defined element is used in a context where it is not permitted, if a required attribute is omitted, or if the content of the element does not correspond to the content that is allowed for the element."),
    ("XTSE0020", "It is a static error if an attribute (other than an attribute written using curly brackets in a position where an attribute value template is permitted) contains a value that is not one of the permitted values for that attribute."),
    ("XTSE0080", "It is a static error to use a reserved namespace in the name of a named template , a mode , an attribute set , a key , a decimal-format , a variable or parameter , a stylesheet function , a named output definition , or a character map ."),
    ("XTSE0090", "It is a static error for an element from the XSLT namespace to have an attribute whose namespace is either null (that is, an attribute with an unprefixed name) or the XSLT namespace, other than attributes defined for the element in this document."),
    ("XTSE0110", "The value of the version attribute must be a number: specifically, it must be a a valid instance of the type xs:decimal as defined in [XML Schema Part 2] ."),
    ("XTSE0120", "An xsl:stylesheet element must not have any text node children."),
    ("XTSE0125", "It is a static error if the value of an [xsl:]default-collation attribute, after resolving against the base URI, contains no URI that the implementation recognizes as a collation URI."),
    ("XTSE0130", "It is a static error if the xsl:stylesheet element has a child element whose name has a null namespace URI."),
    ("XTSE0150", "A literal result element that is used as the outermost element of a simplified stylesheet module must have an xsl:version attribute."),
    ("XTSE0165", "It is a static error if the processor is not able to retrieve the resource identified by the URI reference [ in the href attribute of xsl:include or xsl:import ] , or if the resource that is retrieved does not contain a stylesheet module conforming to this specification."),
    ("XTSE0170", "An xsl:include element must be a top-level element."),
    ("XTSE0180", "It is a static error if a stylesheet module directly or indirectly includes itself."),
    ("XTSE0190", "An xsl:import element must be a top-level element."),
    ("XTSE0200", "The xsl:import element children must precede all other element children of an xsl:stylesheet element, including any xsl:include element children and any user-defined data elements ."),
    ("XTSE0210", "It is a static error if a stylesheet module directly or indirectly imports itself."),
    ("XTSE0215", "It is a static error if an xsl:import-schema element that contains an xs:schema element has a schema-location attribute, or if it has a namespace attribute that conflicts with the target namespace of the contained schema."),
    ("XTSE0220", "It is a static error if the synthetic schema document does not satisfy the constraints described in [XML Schema Part 1] (section 5.1, Errors in Schema Construction and Structure ). This includes, without loss of generality, conflicts such as multiple definitions of the same name."),
    ("XTSE0260", "Within an XSLT element that is required to be empty, any content other than comments or processing instructions, including any whitespace text node preserved using the xml:space=\"preserve\" attribute, is a static error ."),
    ("XTSE0265", "It is a static error if there is a stylesheet module in the stylesheet that specifies input-type-annotations=\"strip\" and another stylesheet module that specifies input-type-annotations=\"preserve\" ."),
    ("XTSE0280", "In the case of a prefixed QName used as the value of an attribute in the stylesheet , or appearing within an XPath expression in the stylesheet, it is a static error if the defining element has no namespace node whose name matches the prefix of the QName ."),
    ("XTSE0340", "Where an attribute is defined to contain a pattern , it is a static error if the pattern does not match the production Pattern ."),
    ("XTSE0350", "It is a static error if an unescaped left curly bracket appears in a fixed part of an attribute value template without a matching right curly bracket."),
    ("XTSE0370", "It is a static error if an unescaped right curly bracket occurs in a fixed part of an attribute value template."),
    ("XTSE0500", "An xsl:template element must have either a match attribute or a name attribute, or both. An xsl:template element that has no match attribute must have no mode attribute and no priority attribute."),
    ("XTSE0530", "The value of this attribute [the priority attribute of the xsl:template element] must conform to the rules for the xs:decimal type defined in [XML Schema Part 2] . Negative values are permitted."),
    ("XTSE0550", "It is a static error if the list [of modes in the mode attribute of xsl:template ] is empty, if the same token is included more than once in the list, if the list contains an invalid token, or if the token #all appears together with any other value."),
    ("XTSE0580", "It is a static error if two parameters of a template or of a stylesheet function have the same name."),
    ("XTSE0620", "It is a static error if a variable-binding element has a select attribute and has non-empty content."),
    ("XTSE0630", "It is a static error if a stylesheet contains more than one binding of a global variable with the same name and same import precedence , unless it also contains another binding with the same name and higher import precedence."),
    ("XTSE0650", "It is a static error if a stylesheet contains an xsl:call-template instruction whose name attribute does not match the name attribute of any xsl:template in the stylesheet ."),
    ("XTSE0660", "It is a static error if a stylesheet contains more than one template with the same name and the same import precedence , unless it also contains a template with the same name and higher import precedence ."),
    ("XTSE0670", "It is a static error if a single xsl:call-template , xsl:apply-templates , xsl:apply-imports , or xsl:next-match element contains two or more xsl:with-param elements with matching name attributes."),
    ("XTSE0680", "In the case of xsl:call-template , it is a static error to pass a non-tunnel parameter named x to a template that does not have a template parameter named x , unless backwards compatible behavior is enabled for the xsl:call-template instruction."),
    ("XTSE0690", "It is a static error if a template that is invoked using xsl:call-template declares a template parameter specifying required=\"yes\" and not specifying tunnel=\"yes\" , if no value for this parameter is supplied by the calling instruction."),
    ("XTSE0710", "It is a static error if the value of the use-attribute-sets attribute of an xsl:copy , xsl:element , or xsl:attribute-set element, or the xsl:use-attribute-sets attribute of a literal result element , is not a whitespace-separated sequence of QNames , or if it contains a QName that does not match the name attribute of any xsl:attribute-set declaration in the stylesheet."),
    ("XTSE0720", "It is a static error if an xsl:attribute-set element directly or indirectly references itself via the names contained in the use-attribute-sets attribute."),
    ("XTSE0740", "A stylesheet function must have a prefixed name, to remove any risk of a clash with a function in the default function namespace. It is a static error if the name has no prefix."),
    ("XTSE0760", "Because arguments to a stylesheet function call must all be specified, the xsl:param elements within an xsl:function element must not specify a default value: this means they must be empty, and must not have a select attribute."),
    ("XTSE0770", "It is a static error for a stylesheet to contain two or more functions with the same expanded-QName , the same arity , and the same import precedence , unless there is another function with the same expanded-QName and arity, and a higher import precedence."),
    ("XTSE0805", "It is a static error if an attribute on a literal result element is in the XSLT namespace , unless it is one of the attributes explicitly defined in this specification."),
    ("XTSE0808", "It is a static error if a namespace prefix is used within the [xsl:]exclude-result-prefixes attribute and there is no namespace binding in scope for that prefix."),
    ("XTSE0809", "It is a static error if the value #default is used within the [xsl:]exclude-result-prefixes attribute and the parent element of the [xsl:]exclude-result-prefixes attribute has no default namespace."),
    ("XTSE0810", "It is a static error if there is more than one such declaration [more than one xsl:namespace-alias declaration] with the same literal namespace URI and the same import precedence and different values for the target namespace URI , unless there is also an xsl:namespace-alias declaration with the same literal namespace URI and a higher import precedence."),
    ("XTSE0812", "It is a static error if a value other than #default is specified for either the stylesheet-prefix or the result-prefix attributes of the xsl:namespace-alias element when there is no in-scope binding for that namespace prefix."),
    ("XTSE0840", "It is a static error if the select attribute of the xsl:attribute element is present unless the element has empty content."),
    ("XTSE0870", "It is a static error if the select attribute of the xsl:value-of element is present when the content of the element is non-empty, or if the select attribute is absent when the content is empty."),
    ("XTSE0880", "It is a static error if the select attribute of the xsl:processing-instruction element is present unless the element has empty content."),
    ("XTSE0910", "It is a static error if the select attribute of the xsl:namespace element is present when the element has content other than one or more xsl:fallback instructions, or if the select attribute is absent when the element has empty content."),
    ("XTSE0940", "It is a static error if the select attribute of the xsl:comment element is present unless the element has empty content."),
    ("XTSE0975", "It is a static error if the value attribute of xsl:number is present unless the select , level , count , and from attributes are all absent."),
    ("XTSE1015", "It is a static error if an xsl:sort element with a select attribute has non-empty content."),
    ("XTSE1017", "It is a static error if an xsl:sort element other than the first in a sequence of sibling xsl:sort elements has a stable attribute."),
    ("XTSE1040", "It is a static error if an xsl:perform-sort instruction with a select attribute has any content other than xsl:sort and xsl:fallback instructions."),
    ("XTSE1060", "It is a static error if the current-group function is used within a pattern ."),
    ("XTSE1070", "It is a static error if the current-grouping-key function is used within a pattern ."),
    ("XTSE1080", "These four attributes [the group-by , group-adjacent , group-starting-with , and group-ending-with attributes of xsl:for-each-group ] are mutually exclusive: it is a static error if none of these four attributes is present, or if more than one of them is present."),
    ("XTSE1090", "It is an error to specify the collation attribute if neither the group-by attribute nor group-adjacent attribute is specified."),
    ("XTSE1130", "It is a static error if the xsl:analyze-string instruction contains neither an xsl:matching-substring nor an xsl:non-matching-substring element."),
    ("XTSE1205", "It is a static error if an xsl:key declaration has a use attribute and has non-empty content, or if it has empty content and no use attribute."),
    ("XTSE1210", "It is a static error if the xsl:key declaration has a collation attribute whose value (after resolving against the base URI) is not a URI recognized by the implementation as referring to a collation."),
    ("XTSE1220", "It is a static error if there are several xsl:key declarations in the stylesheet with the same key name and different effective collations. Two collations are the same if their URIs are equal under the rules for comparing xs:anyURI values, or if the implementation can determine that they are different URIs referring to the same collation."),
    ("XTSE1290", "It is a static error if a named or unnamed decimal format contains two conflicting values for the same attribute in different xsl:decimal-format declarations having the same import precedence , unless there is another definition of the same attribute with higher import precedence."),
    ("XTSE1295", "It is a static error if the character specified in the zero-digit attribute is not a digit or is a digit that does not have the numeric value zero."),
    ("XTSE1300", "It is a static error if, for any named or unnamed decimal format, the variables representing characters used in a picture string do not each have distinct values. These variables are decimal-separator-sign , grouping-sign , percent-sign , per-mille-sign , digit-zero-sign , digit-sign , and pattern-separator-sign ."),
    ("XTSE1430", "It is a static error if there is no namespace bound to the prefix on the element bearing the [xsl:]extension-element-prefixes attribute or, when #default is specified, if there is no default namespace."),
    ("XTSE1505", "It is a static error if both the [xsl:]type and [xsl:]validation attributes are present on the xsl:element , xsl:attribute , xsl:copy , xsl:copy-of , xsl:document , or xsl:result-document instructions, or on a literal result element ."),
    ("XTSE1520", "It is a static error if the value of the type attribute of an xsl:element , xsl:attribute , xsl:copy , xsl:copy-of , xsl:document , or xsl:result-document instruction, or the xsl:type attribute of a literal result element, is not a valid QName , or if it uses a prefix that is not defined in an in-scope namespace declaration, or if the QName is not the name of a type definition included in the in-scope schema components for the stylesheet."),
    ("XTSE1530", "It is a static error if the value of the type attribute of an xsl:attribute instruction refers to a complex type definition"),
    ("XTSE1560", "It is a static error if two xsl:output declarations within an output definition specify explicit values for the same attribute (other than cdata-section-elements and use-character-maps ), with the values of the attributes being not equal, unless there is another xsl:output declaration within the same output definition that has higher import precedence and that specifies an explicit value for the same attribute."),
    ("XTSE1570", "The value [of the method attribute on xsl:output ] must (if present) be a valid QName . If the QName does not have a prefix, then it identifies a method specified in [XSLT and XQuery Serialization] and must be one of xml , html , xhtml , or text ."),
    ("XTSE1580", "It is a static error if the stylesheet contains two or more character maps with the same name and the same import precedence , unless it also contains another character map with the same name and higher import precedence."),
    ("XTSE1590", "It is a static error if a name in the use-character-maps attribute of the xsl:output or xsl:character-map elements does not match the name attribute of any xsl:character-map in the stylesheet ."),
    ("XTSE1600", "It is a static error if a character map references itself, directly or indirectly, via a name in the use-character-maps attribute."),
    ("XTSE1650", "A basic XSLT processor must signal a static error if the stylesheet includes an xsl:import-schema declaration."),
    ("XTSE1660", "A basic XSLT processor must signal a static error if the stylesheet includes an [xsl:]type attribute, or an [xsl:]validation or default-validation attribute with a value other than strip . Type errors"),
    ("XTTE0505", "It is a type error if the result of evaluating the sequence constructor cannot be converted to the required type."),
    ("XTTE0510", "It is a type error if an xsl:apply-templates instruction with no select attribute is evaluated when the context item is not a node."),
    ("XTTE0520", "It is a type error if the sequence returned by the select expression [of xsl:apply-templates ] contains an item that is not a node."),
    ("XTTE0570", "It is a type error if the supplied value of a variable cannot be converted to the required type."),
    ("XTTE0590", "It is a type error if the conversion of the supplied value of a parameter to its required type fails."),
    ("XTTE0600", "If a default value is given explicitly, that is, if there is either a select attribute or a non-empty sequence constructor , then it is a type error if the default value cannot be converted to the required type, using the function conversion rules ."),
    ("XTTE0780", "If the as attribute [of xsl:function ] is specified, then the result evaluated by the sequence constructor (see 5.7 Sequence Constructors ) is converted to the required type, using the function conversion rules . It is a type error if this conversion fails."),
    ("XTTE0790", "If the value of a parameter to a stylesheet function cannot be converted to the required type, a type error is signaled."),
    ("XTTE0950", "It is a type error to use the xsl:copy or xsl:copy-of instruction to copy a node that has namespace-sensitive content if the copy-namespaces attribute has the value no and its explicit or implicit validation attribute has the value preserve . It is also a type error if either of these instructions (with validation=\"preserve\" ) is used to copy an attribute having namespace-sensitive content, unless the parent element is also copied. A node has namespace-sensitive content if its typed value contains an item of type xs:QName or xs:NOTATION or a type derived therefrom. The reason this is an error "),
    ("XTTE0990", "It is a type error if the xsl:number instruction is evaluated, with no value or select attribute, when the context item is not a node."),
    ("XTTE1000", "It is a type error if the result of evaluating the select attribute of the xsl:number instruction is anything other than a single node."),
    ("XTTE1020", "If any sort key value , after atomization and any type conversion required by the data-type attribute, is a sequence containing more than one item, then the effect depends on whether the xsl:sort element is evaluated with backwards compatible behavior . With backwards compatible behavior, the effective sort key value is the first item in the sequence. In other cases, this is a type error ."),
    ("XTTE1100", "It is a type error if the grouping key evaluated using the group-adjacent attribute is an empty sequence, or a sequence containing more than one item."),
    ("XTTE1120", "When the group-starting-with or group-ending-with attribute [of the xsl:for-each-group instruction] is used, it is a type error if the result of evaluating the select expression contains an item that is not a node."),
    ("XTTE1510", "If the validation attribute of an xsl:element , xsl:attribute , xsl:copy , xsl:copy-of , or xsl:result-document instruction, or the xsl:validation attribute of a literal result element, has the effective value strict , and schema validity assessment concludes that the validity of the element or attribute is invalid or unknown, a type error occurs. As with other type errors, the error may be signaled statically if it can be detected statically."),
    ("XTTE1512", "If the validation attribute of an xsl:element , xsl:attribute , xsl:copy , xsl:copy-of , or xsl:result-document instruction, or the xsl:validation attribute of a literal result element, has the effective value strict , and there is no matching top-level declaration in the schema, then a type error occurs. As with other type errors, the error may be signaled statically if it can be detected statically."),
    ("XTTE1515", "If the validation attribute of an xsl:element , xsl:attribute , xsl:copy , xsl:copy-of , or xsl:result-document instruction, or the xsl:validation attribute of a literal result element, has the effective value lax , and schema validity assessment concludes that the element or attribute is invalid, a type error occurs. As with other type errors, the error may be signaled statically if it can be detected statically."),
    ("XTTE1540", "It is a type error if an [xsl:]type attribute is defined for a constructed element or attribute, and the outcome of schema validity assessment against that type is that the validity property of that element or attribute information item is other than valid ."),
    ("XTTE1545", "A type error occurs if a type or validation attribute is defined (explicitly or implicitly) for an instruction that constructs a new attribute node, if the effect of this is to cause the attribute value to be validated against a type that is derived from, or constructed by list or union from, the primitive types xs:QName or xs:NOTATION ."),
    ("XTTE1550", "A type error occurs [when a document node is validated] unless the children of the document node comprise exactly one element node, no text nodes, and zero or more comment and processing instruction nodes, in any order."),
    ("XTTE1555", "It is a type error if, when validating a document node, document-level constraints are not satisfied. These constraints include identity constraints ( xs:unique , xs:key , and xs:keyref ) and ID/IDREF constraints. Dynamic errors"),
];

/// It is a non-recoverable dynamic error if the effective value of an attribute written using curly brackets, in a position where an attribute value template is permitted, is a value that is not one of the permitted values …
pub const XTDE0030: &str = "XTDE0030";
/// It is a non-recoverable dynamic error if the invocation of the stylesheet specifies a template name that does not match the expanded-QName of a named template defined in the stylesheet .
pub const XTDE0040: &str = "XTDE0040";
/// It is a non-recoverable dynamic error if the invocation of the stylesheet specifies an initial mode (other than the default mode) that does not match the expanded-QName in the mode attribute of any template defined in th…
pub const XTDE0045: &str = "XTDE0045";
/// It is a non-recoverable dynamic error if the invocation of the stylesheet specifies both an initial mode and an initial template.
pub const XTDE0047: &str = "XTDE0047";
/// It is a non-recoverable dynamic error if the stylesheet that is invoked declares a visible stylesheet parameter with required="yes" and no value for this parameter is supplied during the invocation of the stylesheet
pub const XTDE0050: &str = "XTDE0050";
/// It is a non-recoverable dynamic error if the initial template defines a template parameter that specifies required="yes" .
pub const XTDE0060: &str = "XTDE0060";
/// If an implementation does not support backwards-compatible behavior, then it is a non-recoverable dynamic error if any element is evaluated that enables backwards-compatible behavior.
pub const XTDE0160: &str = "XTDE0160";
/// Where the result of evaluating an XPath expression (or an attribute value template) is required to be a lexical QName , then unless otherwise specified it is a non-recoverable dynamic error if the defining element has no…
pub const XTDE0290: &str = "XTDE0290";
/// It is a non-recoverable dynamic error if the result sequence used to construct the content of an element node contains a namespace node or attribute node that is preceded in the sequence by a node that is neither a names…
pub const XTDE0410: &str = "XTDE0410";
/// It is a non-recoverable dynamic error if the result sequence used to construct the content of a document node contains a namespace node or attribute node.
pub const XTDE0420: &str = "XTDE0420";
/// It is a non-recoverable dynamic error if the result sequence contains two or more namespace nodes having the same name but different string values (that is, namespace nodes that map the same prefix to different namespace…
pub const XTDE0430: &str = "XTDE0430";
/// It is a non-recoverable dynamic error if the result sequence contains a namespace node with no name and the element node being constructed has a null namespace URI (that is, it is an error to define a default namespace w…
pub const XTDE0440: &str = "XTDE0440";
/// It is a non-recoverable dynamic error if xsl:apply-imports or xsl:next-match is evaluated when the current template rule is null.
pub const XTDE0560: &str = "XTDE0560";
/// If an optional parameter has no select attribute and has an empty sequence constructor , and if there is an as attribute, then the default value of the parameter is an empty sequence
pub const XTDE0610: &str = "XTDE0610";
/// In general, a circularity in a stylesheet is a non-recoverable dynamic error .
pub const XTDE0640: &str = "XTDE0640";
/// In other cases, [with xsl:apply-templates , xsl:apply-imports , and xsl:next-match , or xsl:call-template with tunnel parameters ] it is a non-recoverable dynamic error if the template that is invoked declares a template…
pub const XTDE0700: &str = "XTDE0700";
/// It is a non-recoverable dynamic error if the effective value of the name attribute [of the xsl:element instruction] is not a lexical QName .
pub const XTDE0820: &str = "XTDE0820";
/// In the case of an xsl:element instruction with no namespace attribute, it is a non-recoverable dynamic error if the effective value of the name attribute is a QName whose prefix is not declared in an in-scope namespace d…
pub const XTDE0830: &str = "XTDE0830";
/// It is a non-recoverable dynamic error if the effective value of the namespace attribute [of the xsl:element instruction] is not in the lexical space of the xs:anyURI data type or if it is the string `http://www.w3.org/200…`
pub const XTDE0835: &str = "XTDE0835";
/// It is a non-recoverable dynamic error if the effective value of the name attribute [of an xsl:attribute instruction] is not a lexical QName .
pub const XTDE0850: &str = "XTDE0850";
/// In the case of an xsl:attribute instruction with no namespace attribute, it is a non-recoverable dynamic error if the effective value of the name attribute is the string xmlns .
pub const XTDE0855: &str = "XTDE0855";
/// In the case of an xsl:attribute instruction with no namespace attribute, it is a non-recoverable dynamic error if the effective value of the name attribute is a lexical QName whose prefix is not declared in an in-scope n…
pub const XTDE0860: &str = "XTDE0860";
/// It is a non-recoverable dynamic error if the effective value of the namespace attribute [of the xsl:attribute instruction] is not in the lexical space of the xs:anyURI data type or if it is the string `http://www.w3.org/2…`
pub const XTDE0865: &str = "XTDE0865";
/// It is a non-recoverable dynamic error if the effective value of the name attribute [of the xsl:processing-instruction instruction] is not both an NCName Names and a PITarget XML .
pub const XTDE0890: &str = "XTDE0890";
/// It is a non-recoverable dynamic error if the string value of the new namespace node is not valid in the lexical space of the data type xs:anyURI , or if it is the string <http://www.w3.org/2000/xmlns/> .
pub const XTDE0905: &str = "XTDE0905";
/// It is a non-recoverable dynamic error if the effective value of the name attribute [of the xsl:namespace instruction] is neither a zero-length string nor an NCName Names , or if it is xmlns .
pub const XTDE0920: &str = "XTDE0920";
/// It is a non-recoverable dynamic error if the xsl:namespace instruction generates a namespace node whose name is xml and whose string value is not <http://www.w3.org/XML/1998/namespace> , or a namespace node whose string va…
pub const XTDE0925: &str = "XTDE0925";
/// It is a non-recoverable dynamic error if evaluating the select attribute or the contained sequence constructor of an xsl:namespace instruction results in a zero-length string.
pub const XTDE0930: &str = "XTDE0930";
/// It is a non-recoverable dynamic error if any undiscarded item in the atomized sequence supplied as the value of the value attribute of xsl:number cannot be converted to an integer, or if the resulting integer is less tha…
pub const XTDE0980: &str = "XTDE0980";
/// It is a non-recoverable dynamic error if, for any sort key component , the set of sort key values evaluated for all the items in the initial sequence , after any type conversion requested, contains a pair of ordinary val…
pub const XTDE1030: &str = "XTDE1030";
/// It is a non-recoverable dynamic error if the collation attribute of xsl:sort (after resolving against the base URI) is not a URI that is recognized by the implementation as referring to a collation.
pub const XTDE1035: &str = "XTDE1035";
/// It is a non-recoverable dynamic error if the collation URI specified to xsl:for-each-group (after resolving against the base URI) is a collation that is not recognized by the implementation
pub const XTDE1110: &str = "XTDE1110";
/// It is a non-recoverable dynamic error if the effective value of the regex attribute [of the xsl:analyze-string instruction] does not conform to the required syntax for regular expressions, as specified in [Functions and …
pub const XTDE1140: &str = "XTDE1140";
/// It is a non-recoverable dynamic error if the effective value of the flags attribute [of the xsl:analyze-string instruction] has a value other than the values defined in [Functions and Operators]
pub const XTDE1145: &str = "XTDE1145";
/// It is a non-recoverable dynamic error if the effective value of the regex attribute [of the xsl:analyze-string instruction] is a regular expression that matches a zero-length string: or more specifically, if the regular …
pub const XTDE1150: &str = "XTDE1150";
/// It is a non-recoverable dynamic error if a URI [supplied in the first argument to the unparsed-text function] contains a fragment identifier, or if it cannot be used to retrieve a resource containing text.
pub const XTDE1170: &str = "XTDE1170";
/// It is a non-recoverable dynamic error if a resource [retrieved using the unparsed-text function] contains octets that cannot be decoded into Unicode characters using the specified encoding, or if the resulting characters…
pub const XTDE1190: &str = "XTDE1190";
/// It is a non-recoverable dynamic error if the second argument of the unparsed-text function is omitted and the processor cannot infer the encoding using external information and the encoding is not UTF-8.
pub const XTDE1200: &str = "XTDE1200";
/// It is a non-recoverable dynamic error if the value [of the first argument to the key function] is not a valid QName, or if there is no namespace declaration in scope for the prefix of the QName, or if the name obtained b…
pub const XTDE1260: &str = "XTDE1260";
/// It is a non-recoverable dynamic error to call the key function with two arguments if there is no context node , or if the root of the tree containing the context node is not a document node; or to call the function with …
pub const XTDE1270: &str = "XTDE1270";
/// It is a non-recoverable dynamic error if the name specified as the $decimal-format-name argument [ to the format-number function] is not a valid QName , or if its prefix has not been declared in an in-scope namespace dec…
pub const XTDE1280: &str = "XTDE1280";
/// The picture string [supplied to the format-number function] must conform to the following rules
pub const XTDE1310: &str = "XTDE1310";
/// It is a non-recoverable dynamic error if the syntax of the picture [used for date/time formatting] is incorrect.
pub const XTDE1340: &str = "XTDE1340";
/// It is a non-recoverable dynamic error if a component specifier within the picture [used for date/time formatting] refers to components that are not available in the given type of $value , for example if the picture suppl…
pub const XTDE1350: &str = "XTDE1350";
/// If the current function is evaluated within an expression that is evaluated when the context item is undefined, a non-recoverable dynamic error occurs.
pub const XTDE1360: &str = "XTDE1360";
/// It is a non-recoverable dynamic error if the unparsed-entity-uri function is called when there is no context node , or when the root of the tree containing the context node is not a document node.
pub const XTDE1370: &str = "XTDE1370";
/// It is a non-recoverable dynamic error if the unparsed-entity-public-id function is called when there is no context node , or when the root of the tree containing the context node is not a document node.
pub const XTDE1380: &str = "XTDE1380";
/// It is a non-recoverable dynamic error if the value [supplied as the $property-name argument to the system-property function] is not a valid QName, or if there is no namespace declaration in scope for the prefix of the QN…
pub const XTDE1390: &str = "XTDE1390";
/// It is a non-recoverable dynamic error if the argument [passed to the function-available function] does not evaluate to a string that is a valid QName , or if there is no namespace declaration in scope for the prefix of t…
pub const XTDE1400: &str = "XTDE1400";
/// It is a non-recoverable dynamic error if the arguments supplied to a call on an extension function do not satisfy the rules defined for that particular extension function, or if the extension function reports an error, o…
pub const XTDE1420: &str = "XTDE1420";
/// When backwards compatible behavior is enabled, it is a non-recoverable dynamic error to evaluate an extension function call if no implementation of the extension function is available.
pub const XTDE1425: &str = "XTDE1425";
/// It is a non-recoverable dynamic error if the argument [passed to the type-available function] does not evaluate to a string that is a valid QName , or if there is no namespace declaration in scope for the prefix of the Q…
pub const XTDE1428: &str = "XTDE1428";
/// It is a non-recoverable dynamic error if the argument [passed to the element-available function] does not evaluate to a string that is a valid QName , or if there is no namespace declaration in scope for the prefix of th…
pub const XTDE1440: &str = "XTDE1440";
/// When a processor performs fallback for an extension instruction that is not recognized, if the instruction element has one or more xsl:fallback children, then the content of each of the xsl:fallback children must be eval…
pub const XTDE1450: &str = "XTDE1450";
/// It is a non-recoverable dynamic error if the effective value of the format attribute [of an xsl:result-document element] is not a valid lexical QName , or if it does not match the expanded-QName of an output definition i…
pub const XTDE1460: &str = "XTDE1460";
/// It is a non-recoverable dynamic error to evaluate the xsl:result-document instruction in temporary output state .
pub const XTDE1480: &str = "XTDE1480";
/// It is a non-recoverable dynamic error for a transformation to generate two or more final result trees with the same URI.
pub const XTDE1490: &str = "XTDE1490";
/// A basic XSLT processor must raise a non-recoverable dynamic error if the input to the processor includes a node with a type annotation other than xs:untyped or xs:untypedAtomic , or an atomic value of a type other than t…
pub const XTDE1665: &str = "XTDE1665";
/// When a transformation is terminated by use of xsl:message terminate="yes" , the effect is the same as when a non-recoverable dynamic error occurs during the transformation.
pub const XTMM9000: &str = "XTMM9000";
/// It is a recoverable dynamic error if this [the process of finding an xsl:strip-space or xsl:preserve-space declaration to match an element in the source document] leaves more than one match, unless all the matched declar…
pub const XTRE0270: &str = "XTRE0270";
/// It is a recoverable dynamic error if the conflict resolution algorithm for template rules leaves more than one matching template rule
pub const XTRE0540: &str = "XTRE0540";
/// It is a recoverable dynamic error if the name of a constructed attribute is xml:space and the value is not either default or preserve
pub const XTRE0795: &str = "XTRE0795";
/// When a URI reference [supplied to the document function] contains a fragment identifier, it is a recoverable dynamic error if the media type is not one that is recognized by the processor, or if the fragment identifier d…
pub const XTRE1160: &str = "XTRE1160";
/// It is a recoverable dynamic error for a transformation to generate two or more final result trees with URIs that identify the same physical resource
pub const XTRE1495: &str = "XTRE1495";
/// It is a recoverable dynamic error for a stylesheet to write to an external resource and read from the same resource during a single transformation, whether or not the same URI is used to access the resource in both cases
pub const XTRE1500: &str = "XTRE1500";
/// It is a recoverable dynamic error if an xsl:value-of or xsl:text instruction specifies that output escaping is to be disabled and the implementation does not support this
pub const XTRE1620: &str = "XTRE1620";
/// It is a recoverable dynamic error if an xsl:value-of or xsl:text instruction specifies that output escaping is to be disabled when writing to a final result tree that is not being serialized
pub const XTRE1630: &str = "XTRE1630";
/// A static error is signaled if an XSLT-defined element is used in a context where it is not permitted, if a required attribute is omitted, or if the content of the element does not correspond to the content that is allowe…
pub const XTSE0010: &str = "XTSE0010";
/// It is a static error if an attribute (other than an attribute written using curly brackets in a position where an attribute value template is permitted) contains a value that is not one of the permitted values for that a…
pub const XTSE0020: &str = "XTSE0020";
/// It is a static error to use a reserved namespace in the name of a named template , a mode , an attribute set , a key , a decimal-format , a variable or parameter , a stylesheet function , a named output definition , or a…
pub const XTSE0080: &str = "XTSE0080";
/// It is a static error for an element from the XSLT namespace to have an attribute whose namespace is either null (that is, an attribute with an unprefixed name) or the XSLT namespace, other than attributes defined for the…
pub const XTSE0090: &str = "XTSE0090";
/// The value of the version attribute must be a number: specifically, it must be a a valid instance of the type xs:decimal as defined in [XML Schema Part 2] .
pub const XTSE0110: &str = "XTSE0110";
/// An xsl:stylesheet element must not have any text node children.
pub const XTSE0120: &str = "XTSE0120";
/// It is a static error if the value of an \[xsl:\]default-collation attribute, after resolving against the base URI, contains no URI that the implementation recognizes as a collation URI.
pub const XTSE0125: &str = "XTSE0125";
/// It is a static error if the xsl:stylesheet element has a child element whose name has a null namespace URI.
pub const XTSE0130: &str = "XTSE0130";
/// A literal result element that is used as the outermost element of a simplified stylesheet module must have an xsl:version attribute.
pub const XTSE0150: &str = "XTSE0150";
/// It is a static error if the processor is not able to retrieve the resource identified by the URI reference [ in the href attribute of xsl:include or xsl:import ] , or if the resource that is retrieved does not contain a …
pub const XTSE0165: &str = "XTSE0165";
/// An xsl:include element must be a top-level element.
pub const XTSE0170: &str = "XTSE0170";
/// It is a static error if a stylesheet module directly or indirectly includes itself.
pub const XTSE0180: &str = "XTSE0180";
/// An xsl:import element must be a top-level element.
pub const XTSE0190: &str = "XTSE0190";
/// The xsl:import element children must precede all other element children of an xsl:stylesheet element, including any xsl:include element children and any user-defined data elements .
pub const XTSE0200: &str = "XTSE0200";
/// It is a static error if a stylesheet module directly or indirectly imports itself.
pub const XTSE0210: &str = "XTSE0210";
/// It is a static error if an xsl:import-schema element that contains an xs:schema element has a schema-location attribute, or if it has a namespace attribute that conflicts with the target namespace of the contained schema…
pub const XTSE0215: &str = "XTSE0215";
/// It is a static error if the synthetic schema document does not satisfy the constraints described in [XML Schema Part 1] (section 5.1, Errors in Schema Construction and Structure )
pub const XTSE0220: &str = "XTSE0220";
/// Within an XSLT element that is required to be empty, any content other than comments or processing instructions, including any whitespace text node preserved using the xml:space="preserve" attribute, is a static error .
pub const XTSE0260: &str = "XTSE0260";
/// It is a static error if there is a stylesheet module in the stylesheet that specifies input-type-annotations="strip" and another stylesheet module that specifies input-type-annotations="preserve" .
pub const XTSE0265: &str = "XTSE0265";
/// In the case of a prefixed QName used as the value of an attribute in the stylesheet , or appearing within an XPath expression in the stylesheet, it is a static error if the defining element has no namespace node whose na…
pub const XTSE0280: &str = "XTSE0280";
/// Where an attribute is defined to contain a pattern , it is a static error if the pattern does not match the production Pattern .
pub const XTSE0340: &str = "XTSE0340";
/// It is a static error if an unescaped left curly bracket appears in a fixed part of an attribute value template without a matching right curly bracket.
pub const XTSE0350: &str = "XTSE0350";
/// It is a static error if an unescaped right curly bracket occurs in a fixed part of an attribute value template.
pub const XTSE0370: &str = "XTSE0370";
/// An xsl:template element must have either a match attribute or a name attribute, or both
pub const XTSE0500: &str = "XTSE0500";
/// The value of this attribute [the priority attribute of the xsl:template element] must conform to the rules for the xs:decimal type defined in [XML Schema Part 2]
pub const XTSE0530: &str = "XTSE0530";
/// It is a static error if the list [of modes in the mode attribute of xsl:template ] is empty, if the same token is included more than once in the list, if the list contains an invalid token, or if the token #all appears t…
pub const XTSE0550: &str = "XTSE0550";
/// It is a static error if two parameters of a template or of a stylesheet function have the same name.
pub const XTSE0580: &str = "XTSE0580";
/// It is a static error if a variable-binding element has a select attribute and has non-empty content.
pub const XTSE0620: &str = "XTSE0620";
/// It is a static error if a stylesheet contains more than one binding of a global variable with the same name and same import precedence , unless it also contains another binding with the same name and higher import preced…
pub const XTSE0630: &str = "XTSE0630";
/// It is a static error if a stylesheet contains an xsl:call-template instruction whose name attribute does not match the name attribute of any xsl:template in the stylesheet .
pub const XTSE0650: &str = "XTSE0650";
/// It is a static error if a stylesheet contains more than one template with the same name and the same import precedence , unless it also contains a template with the same name and higher import precedence .
pub const XTSE0660: &str = "XTSE0660";
/// It is a static error if a single xsl:call-template , xsl:apply-templates , xsl:apply-imports , or xsl:next-match element contains two or more xsl:with-param elements with matching name attributes.
pub const XTSE0670: &str = "XTSE0670";
/// In the case of xsl:call-template , it is a static error to pass a non-tunnel parameter named x to a template that does not have a template parameter named x , unless backwards compatible behavior is enabled for the xsl:c…
pub const XTSE0680: &str = "XTSE0680";
/// It is a static error if a template that is invoked using xsl:call-template declares a template parameter specifying required="yes" and not specifying tunnel="yes" , if no value for this parameter is supplied by the calli…
pub const XTSE0690: &str = "XTSE0690";
/// It is a static error if the value of the use-attribute-sets attribute of an xsl:copy , xsl:element , or xsl:attribute-set element, or the xsl:use-attribute-sets attribute of a literal result element , is not a whitespace…
pub const XTSE0710: &str = "XTSE0710";
/// It is a static error if an xsl:attribute-set element directly or indirectly references itself via the names contained in the use-attribute-sets attribute.
pub const XTSE0720: &str = "XTSE0720";
/// A stylesheet function must have a prefixed name, to remove any risk of a clash with a function in the default function namespace
pub const XTSE0740: &str = "XTSE0740";
/// Because arguments to a stylesheet function call must all be specified, the xsl:param elements within an xsl:function element must not specify a default value: this means they must be empty, and must not have a select att…
pub const XTSE0760: &str = "XTSE0760";
/// It is a static error for a stylesheet to contain two or more functions with the same expanded-QName , the same arity , and the same import precedence , unless there is another function with the same expanded-QName and ar…
pub const XTSE0770: &str = "XTSE0770";
/// It is a static error if an attribute on a literal result element is in the XSLT namespace , unless it is one of the attributes explicitly defined in this specification.
pub const XTSE0805: &str = "XTSE0805";
/// It is a static error if a namespace prefix is used within the \[xsl:\]exclude-result-prefixes attribute and there is no namespace binding in scope for that prefix.
pub const XTSE0808: &str = "XTSE0808";
/// It is a static error if the value #default is used within the \[xsl:\]exclude-result-prefixes attribute and the parent element of the \[xsl:\]exclude-result-prefixes attribute has no default namespace.
pub const XTSE0809: &str = "XTSE0809";
/// It is a static error if there is more than one such declaration [more than one xsl:namespace-alias declaration] with the same literal namespace URI and the same import precedence and different values for the target names…
pub const XTSE0810: &str = "XTSE0810";
/// It is a static error if a value other than #default is specified for either the stylesheet-prefix or the result-prefix attributes of the xsl:namespace-alias element when there is no in-scope binding for that namespace pr…
pub const XTSE0812: &str = "XTSE0812";
/// It is a static error if the select attribute of the xsl:attribute element is present unless the element has empty content.
pub const XTSE0840: &str = "XTSE0840";
/// It is a static error if the select attribute of the xsl:value-of element is present when the content of the element is non-empty, or if the select attribute is absent when the content is empty.
pub const XTSE0870: &str = "XTSE0870";
/// It is a static error if the select attribute of the xsl:processing-instruction element is present unless the element has empty content.
pub const XTSE0880: &str = "XTSE0880";
/// It is a static error if the select attribute of the xsl:namespace element is present when the element has content other than one or more xsl:fallback instructions, or if the select attribute is absent when the element ha…
pub const XTSE0910: &str = "XTSE0910";
/// It is a static error if the select attribute of the xsl:comment element is present unless the element has empty content.
pub const XTSE0940: &str = "XTSE0940";
/// It is a static error if the value attribute of xsl:number is present unless the select , level , count , and from attributes are all absent.
pub const XTSE0975: &str = "XTSE0975";
/// It is a static error if an xsl:sort element with a select attribute has non-empty content.
pub const XTSE1015: &str = "XTSE1015";
/// It is a static error if an xsl:sort element other than the first in a sequence of sibling xsl:sort elements has a stable attribute.
pub const XTSE1017: &str = "XTSE1017";
/// It is a static error if an xsl:perform-sort instruction with a select attribute has any content other than xsl:sort and xsl:fallback instructions.
pub const XTSE1040: &str = "XTSE1040";
/// It is a static error if the current-group function is used within a pattern .
pub const XTSE1060: &str = "XTSE1060";
/// It is a static error if the current-grouping-key function is used within a pattern .
pub const XTSE1070: &str = "XTSE1070";
/// These four attributes [the group-by , group-adjacent , group-starting-with , and group-ending-with attributes of xsl:for-each-group ] are mutually exclusive: it is a static error if none of these four attributes is prese…
pub const XTSE1080: &str = "XTSE1080";
/// It is an error to specify the collation attribute if neither the group-by attribute nor group-adjacent attribute is specified.
pub const XTSE1090: &str = "XTSE1090";
/// It is a static error if the xsl:analyze-string instruction contains neither an xsl:matching-substring nor an xsl:non-matching-substring element.
pub const XTSE1130: &str = "XTSE1130";
/// It is a static error if an xsl:key declaration has a use attribute and has non-empty content, or if it has empty content and no use attribute.
pub const XTSE1205: &str = "XTSE1205";
/// It is a static error if the xsl:key declaration has a collation attribute whose value (after resolving against the base URI) is not a URI recognized by the implementation as referring to a collation.
pub const XTSE1210: &str = "XTSE1210";
/// It is a static error if there are several xsl:key declarations in the stylesheet with the same key name and different effective collations
pub const XTSE1220: &str = "XTSE1220";
/// It is a static error if a named or unnamed decimal format contains two conflicting values for the same attribute in different xsl:decimal-format declarations having the same import precedence , unless there is another de…
pub const XTSE1290: &str = "XTSE1290";
/// It is a static error if the character specified in the zero-digit attribute is not a digit or is a digit that does not have the numeric value zero.
pub const XTSE1295: &str = "XTSE1295";
/// It is a static error if, for any named or unnamed decimal format, the variables representing characters used in a picture string do not each have distinct values
pub const XTSE1300: &str = "XTSE1300";
/// It is a static error if there is no namespace bound to the prefix on the element bearing the \[xsl:\]extension-element-prefixes attribute or, when #default is specified, if there is no default namespace.
pub const XTSE1430: &str = "XTSE1430";
/// It is a static error if both the \[xsl:\]type and \[xsl:\]validation attributes are present on the xsl:element , xsl:attribute , xsl:copy , xsl:copy-of , xsl:document , or xsl:result-document instructions, or on a literal re…
pub const XTSE1505: &str = "XTSE1505";
/// It is a static error if the value of the type attribute of an xsl:element , xsl:attribute , xsl:copy , xsl:copy-of , xsl:document , or xsl:result-document instruction, or the xsl:type attribute of a literal result elemen…
pub const XTSE1520: &str = "XTSE1520";
/// It is a static error if the value of the type attribute of an xsl:attribute instruction refers to a complex type definition
pub const XTSE1530: &str = "XTSE1530";
/// It is a static error if two xsl:output declarations within an output definition specify explicit values for the same attribute (other than cdata-section-elements and use-character-maps ), with the values of the attribute…
pub const XTSE1560: &str = "XTSE1560";
/// The value [of the method attribute on xsl:output ] must (if present) be a valid QName
pub const XTSE1570: &str = "XTSE1570";
/// It is a static error if the stylesheet contains two or more character maps with the same name and the same import precedence , unless it also contains another character map with the same name and higher import precedence…
pub const XTSE1580: &str = "XTSE1580";
/// It is a static error if a name in the use-character-maps attribute of the xsl:output or xsl:character-map elements does not match the name attribute of any xsl:character-map in the stylesheet .
pub const XTSE1590: &str = "XTSE1590";
/// It is a static error if a character map references itself, directly or indirectly, via a name in the use-character-maps attribute.
pub const XTSE1600: &str = "XTSE1600";
/// A basic XSLT processor must signal a static error if the stylesheet includes an xsl:import-schema declaration.
pub const XTSE1650: &str = "XTSE1650";
/// A basic XSLT processor must signal a static error if the stylesheet includes an \[xsl:\]type attribute, or an \[xsl:\]validation or default-validation attribute with a value other than strip
pub const XTSE1660: &str = "XTSE1660";
/// It is a type error if the result of evaluating the sequence constructor cannot be converted to the required type.
pub const XTTE0505: &str = "XTTE0505";
/// It is a type error if an xsl:apply-templates instruction with no select attribute is evaluated when the context item is not a node.
pub const XTTE0510: &str = "XTTE0510";
/// It is a type error if the sequence returned by the select expression [of xsl:apply-templates ] contains an item that is not a node.
pub const XTTE0520: &str = "XTTE0520";
/// It is a type error if the supplied value of a variable cannot be converted to the required type.
pub const XTTE0570: &str = "XTTE0570";
/// It is a type error if the conversion of the supplied value of a parameter to its required type fails.
pub const XTTE0590: &str = "XTTE0590";
/// If a default value is given explicitly, that is, if there is either a select attribute or a non-empty sequence constructor , then it is a type error if the default value cannot be converted to the required type, using th…
pub const XTTE0600: &str = "XTTE0600";
/// If the as attribute [of xsl:function ] is specified, then the result evaluated by the sequence constructor (see 5.7 Sequence Constructors ) is converted to the required type, using the function conversion rules
pub const XTTE0780: &str = "XTTE0780";
/// If the value of a parameter to a stylesheet function cannot be converted to the required type, a type error is signaled.
pub const XTTE0790: &str = "XTTE0790";
/// It is a type error to use the xsl:copy or xsl:copy-of instruction to copy a node that has namespace-sensitive content if the copy-namespaces attribute has the value no and its explicit or implicit validation attribute ha…
pub const XTTE0950: &str = "XTTE0950";
/// It is a type error if the xsl:number instruction is evaluated, with no value or select attribute, when the context item is not a node.
pub const XTTE0990: &str = "XTTE0990";
/// It is a type error if the result of evaluating the select attribute of the xsl:number instruction is anything other than a single node.
pub const XTTE1000: &str = "XTTE1000";
/// If any sort key value , after atomization and any type conversion required by the data-type attribute, is a sequence containing more than one item, then the effect depends on whether the xsl:sort element is evaluated wit…
pub const XTTE1020: &str = "XTTE1020";
/// It is a type error if the grouping key evaluated using the group-adjacent attribute is an empty sequence, or a sequence containing more than one item.
pub const XTTE1100: &str = "XTTE1100";
/// When the group-starting-with or group-ending-with attribute [of the xsl:for-each-group instruction] is used, it is a type error if the result of evaluating the select expression contains an item that is not a node.
pub const XTTE1120: &str = "XTTE1120";
/// If the validation attribute of an xsl:element , xsl:attribute , xsl:copy , xsl:copy-of , or xsl:result-document instruction, or the xsl:validation attribute of a literal result element, has the effective value strict , a…
pub const XTTE1510: &str = "XTTE1510";
/// If the validation attribute of an xsl:element , xsl:attribute , xsl:copy , xsl:copy-of , or xsl:result-document instruction, or the xsl:validation attribute of a literal result element, has the effective value strict , a…
pub const XTTE1512: &str = "XTTE1512";
/// If the validation attribute of an xsl:element , xsl:attribute , xsl:copy , xsl:copy-of , or xsl:result-document instruction, or the xsl:validation attribute of a literal result element, has the effective value lax , and …
pub const XTTE1515: &str = "XTTE1515";
/// It is a type error if an \[xsl:\]type attribute is defined for a constructed element or attribute, and the outcome of schema validity assessment against that type is that the validity property of that element or attribute …
pub const XTTE1540: &str = "XTTE1540";
/// A type error occurs if a type or validation attribute is defined (explicitly or implicitly) for an instruction that constructs a new attribute node, if the effect of this is to cause the attribute value to be validated a…
pub const XTTE1545: &str = "XTTE1545";
/// A type error occurs [when a document node is validated] unless the children of the document node comprise exactly one element node, no text nodes, and zero or more comment and processing instruction nodes, in any order.
pub const XTTE1550: &str = "XTTE1550";
/// It is a type error if, when validating a document node, document-level constraints are not satisfied
pub const XTTE1555: &str = "XTTE1555";

/// Look up the spec description of an error code, if it is an XSLT 2.0 code.
pub fn describe(code: &str) -> Option<&'static str> {
    CODES
        .binary_search_by(|(c, _)| c.cmp(&code))
        .ok()
        .map(|i| CODES[i].1)
}
