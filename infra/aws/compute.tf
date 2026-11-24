data "aws_ami" "ubuntu_arm64" {
  most_recent = true
  owners      = ["099720109477"]

  filter {
    name   = "name"
    values = ["ubuntu/images/hvm-ssd-gp3/ubuntu-noble-24.04-arm64-server-*"]
  }

  filter {
    name   = "architecture"
    values = ["arm64"]
  }
}

resource "aws_instance" "node" {
  ami                         = data.aws_ami.ubuntu_arm64.id
  associate_public_ip_address = true
  instance_type               = var.instance_type
  subnet_id                   = aws_subnet.public.id
  vpc_security_group_ids      = [aws_security_group.node.id]
  iam_instance_profile        = aws_iam_instance_profile.node.name

  user_data = templatefile("${path.module}/../../server/deploy/aws/bootstrap/cloud-init.yaml", {
    region      = var.region
    repo_url    = var.repo_url
    repo_ref    = var.repo_ref
    ssm_prefix  = var.ssm_parameter_prefix
    data_volume = replace(aws_ebs_volume.data.id, "-", "")
  })
  user_data_replace_on_change = false

  metadata_options {
    http_endpoint               = "enabled"
    http_tokens                 = "required"
    http_put_response_hop_limit = 1
  }

  root_block_device {
    volume_type = "gp3"
    volume_size = var.root_volume_gb
    encrypted   = true
    tags        = { Name = "via-root", Backup = "via" }
  }

  tags = { Name = "via-node", Backup = "via" }

  lifecycle {
    ignore_changes = [ami]
  }
}

resource "aws_ebs_volume" "data" {
  availability_zone = aws_subnet.public.availability_zone
  type              = "gp3"
  size              = var.data_volume_gb
  encrypted         = true
  tags              = { Name = "via-data", Backup = "via" }

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_volume_attachment" "data" {
  device_name                    = "/dev/sdf"
  volume_id                      = aws_ebs_volume.data.id
  instance_id                    = aws_instance.node.id
  stop_instance_before_detaching = true
}

resource "aws_eip" "node" {
  domain = "vpc"
  tags   = { Name = "via" }
}

resource "aws_eip_association" "node" {
  instance_id   = aws_instance.node.id
  allocation_id = aws_eip.node.id
}

resource "aws_route53_record" "api" {
  count   = var.route53_zone_id == "" ? 0 : 1
  zone_id = var.route53_zone_id
  name    = var.api_hostname
  type    = "A"
  ttl     = 300
  records = [aws_eip.node.public_ip]
}
