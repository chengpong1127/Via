output "public_ip" {
  description = "Elastic IP of the node. Point the API DNS record here when not using Route53."
  value       = aws_eip.node.public_ip
}

output "instance_id" {
  description = "Instance id for `aws ssm start-session --target`."
  value       = aws_instance.node.id
}

output "artifact_bucket" {
  description = "Private artifact bucket name."
  value       = aws_s3_bucket.artifacts.bucket
}

output "ssm_parameters_to_create" {
  description = "Parameters the owner must create with `aws ssm put-parameter --type SecureString`. Values are never stored in OpenTofu."
  value = [
    "${var.ssm_parameter_prefix}/TDX_CLIENT_ID",
    "${var.ssm_parameter_prefix}/TDX_CLIENT_SECRET",
  ]
}
